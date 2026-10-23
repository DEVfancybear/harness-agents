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
//! prime-agent keeps its children: a message to a child that finished wakes it
//! for a follow-up turn in the same conversation, and the spawn ledger
//! (`<data>/delegation/ledger/<conversation>.json`) brings a conversation's
//! children back - listed, collectable and reachable - when the conversation is
//! opened again, after `/resume` or a worker restart. A child that was running
//! when its worker went away comes back as failed.
//!
//! Children write through the session's [`SharedStore`]: the store stays open while
//! a turn or a child holds a lease on it.

use std::fmt::Write as _;
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use harness_orchestrator::{
    AgentRole, ApplyOutcome, BudgetLedger, BudgetUsage, CheckResult, DelegatedOutcome,
    DelegatedResult, DelegationBudget, DelegationGrants, DirtyReason, FinalApply, GrantAction,
    InputInspection, IntegrationOutcome, OrchestratorError, ResultIntegrator, SchedulerConfig,
    TaskBrief, VerifiedSnapshot, WorkerBackend, WorkerOutcome, WorkerRequest, WorkerScheduler,
    WorkspaceManager, WorktreeRecord,
};
use harness_providers::{CancellationToken, MessageRole, ModelProvider, ProviderMessage};
use harness_runtime::{RunInbox, RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WorktreeRecordRow};
use harness_tools::{
    ApprovalAnswer, ApprovalGate, ApprovalMode, ApprovalProposal, CodingToolAction,
    ExternalToolCatalog, ExternalToolDispatcher, ExternalTools, ToolExecutionService, ToolOutput,
    ToolPolicy, TurnDriver, TurnLimits, TurnObserver, TurnOptions, TurnProgress, TurnStop,
    coding_tool_schemas,
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

/// The longest brief, in characters as the tool schema's `maxLength` counts them.
/// Counted in bytes, a Vietnamese brief of 3 000 characters was refused while
/// the schema allowed 8 192.
const MAX_BRIEF_CHARS: usize = 16 * 1024;

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

/// An `RLM_MAX_DEPTH` that bounds nothing: every child may start children of its
/// own. prime-agent's default is 2; ha's is no limit, and `[agents] max_depth`,
/// `rlmMaxDepth`, `RLM_MAX_DEPTH` or `/rlm-max-depth` set one.
pub const UNLIMITED_RLM_MAX_DEPTH: u32 = harness_orchestrator::UNBOUNDED_DEPTH;

/// The `RLM_MAX_DEPTH` a session runs with when nothing sets one.
pub const DEFAULT_RLM_MAX_DEPTH: u32 = UNLIMITED_RLM_MAX_DEPTH;

/// An `RLM_MAX_DEPTH` as the user reads and writes it: a number, or `unlimited`.
#[must_use]
pub fn describe_max_depth(value: u32) -> String {
    if value == UNLIMITED_RLM_MAX_DEPTH {
        "unlimited".to_owned()
    } else {
        value.to_string()
    }
}

/// Read an `RLM_MAX_DEPTH` the user wrote: a non-negative integer, or
/// `unlimited` (also `none`, `inf`, `-1`).
#[must_use]
pub fn parse_max_depth(text: &str) -> Option<u32> {
    let text = text.trim();
    if matches!(
        text.to_ascii_lowercase().as_str(),
        "unlimited" | "none" | "inf" | "infinite" | "-1"
    ) {
        return Some(UNLIMITED_RLM_MAX_DEPTH);
    }
    text.parse().ok()
}

/// Where the session's `RLM_MAX_DEPTH` came from, in prime-agent's words
/// (`core/rlm-max-depth.ts`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaxDepthSource {
    Chat,
    Global,
    Config,
    Env,
    Default,
}

impl MaxDepthSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Global => "global",
            Self::Config => "config",
            Self::Env => "env",
            Self::Default => "default",
        }
    }
}

/// prime-agent's refusal of a spawn past the depth limit.
fn depth_limit_error(depth: u32, max_depth: u32) -> String {
    format!(
        "RLM recursion depth limit reached (RLM_DEPTH={depth}, RLM_MAX_DEPTH={})",
        describe_max_depth(max_depth)
    )
}

/// The parent's web tools, which a child inherits: its catalog and dispatcher.
pub type ChildWebTools = (ExternalTools, Arc<dyn ExternalToolDispatcher>);

/// The model a child runs on.
#[derive(Clone)]
pub struct ChildModel {
    pub provider: Arc<dyn ModelProvider>,
    /// `provider/id`, as the catalog names it.
    pub reference: String,
    pub price: Option<ModelPrice>,
    /// The thinking level it runs at (`high`), when known: the sidebar and
    /// the agents view show it beside the model.
    pub thinking: Option<String>,
}

/// What a child's model was resolved from, when part of it came from the
/// session's defaults (`subagentDefaultModel`, `subagentDefaultThinking`):
/// changing a default moves the running child too, as the user expects of
/// children that never named a model of their own.
#[derive(Clone)]
pub struct FollowedDefaults {
    /// The model it falls back to: its parent's.
    parent: ChildModel,
    /// The model its spawn named (a verifier's is its parent's), if any.
    model: Option<String>,
    /// The thinking level its spawn named, if any.
    thinking: Option<String>,
}

/// A running child's model, which the runtime asks for at every call: a
/// change of the session's defaults takes effect at the child's next call.
struct LiveModel {
    current: std::sync::RwLock<ChildModel>,
}

impl LiveModel {
    fn new(model: ChildModel) -> Self {
        Self {
            current: std::sync::RwLock::new(model),
        }
    }

    fn get(&self) -> ChildModel {
        match self.current.read() {
            Ok(current) => current.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn set(&self, model: ChildModel) {
        match self.current.write() {
            Ok(mut current) => *current = model,
            Err(poisoned) => *poisoned.into_inner() = model,
        }
    }
}

impl ModelProvider for LiveModel {
    fn capabilities(&self) -> harness_providers::ModelCapabilities {
        self.get().provider.capabilities()
    }

    fn stream(
        &self,
        request: harness_providers::ProviderRequest,
        cancellation: CancellationToken,
    ) -> harness_providers::ProviderFuture {
        self.get().provider.stream(request, cancellation)
    }

    fn stream_events(
        &self,
        request: harness_providers::ProviderRequest,
        cancellation: CancellationToken,
    ) -> harness_providers::ProviderEventStream {
        self.get().provider.stream_events(request, cancellation)
    }
}

/// One model `rlm.find_models` may offer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableModel {
    pub provider: String,
    pub id: String,
    pub name: String,
}

/// prime-agent's `rlm.find_models` defaults: eight matches, at most twenty.
const DEFAULT_RLM_MODEL_SEARCH_LIMIT: u64 = 8;
const MAX_RLM_MODEL_SEARCH_LIMIT: u64 = 20;

/// prime-agent's `normalizeModelSearchText`: lowercase ASCII letters and digits.
fn normalize_model_search_text(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// prime-agent's `findRlmModelMatches`: an exact match, then a prefix, then a
/// substring of the selector, id or name, then alphabetical by selector.
fn find_rlm_model_matches(query: &str, models: &[SearchableModel], limit: usize) -> Vec<Value> {
    let query = normalize_model_search_text(query.trim());
    let mut candidates = models
        .iter()
        .filter_map(|model| {
            let selector = format!("{}/{}", model.provider, model.id);
            let name = if model.name.is_empty() {
                model.id.clone()
            } else {
                model.name.clone()
            };
            let fields =
                [&selector, &model.id, &name].map(|field| normalize_model_search_text(field));
            let score = if query.is_empty() {
                0
            } else if let Some(exact) = fields.iter().position(|field| *field == query) {
                exact
            } else if let Some(prefix) = fields.iter().position(|field| field.starts_with(&query)) {
                3 + prefix
            } else {
                6 + fields.iter().position(|field| field.contains(&query))?
            };
            Some((score, selector, model, name))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, selector, model, name)| {
            json!({
                "provider": model.provider,
                "id": model.id,
                "name": name,
                "selector": selector,
            })
        })
        .collect()
}

/// The judges of a child's own turn - its tool results and its last answer -
/// handed to every child's turn, as the root's turn has them.
#[derive(Clone)]
pub struct TurnJudges {
    pub visibility: Arc<dyn harness_tools::ResultVisibility>,
    pub admission: Arc<dyn harness_tools::OutputAdmission>,
    pub completion: Arc<dyn harness_tools::CompletionCheck>,
}

/// A host's judgments about a child before it starts (the jev plugin's
/// subgoal and routing decisions). Each only narrows what happens: a judge that
/// cannot answer says `None` or `false`, and the child starts as without one.
pub trait ChildJudge: Send + Sync {
    /// The one of `existing` (siblings' briefs) that already asks for `brief`'s
    /// work, so its answer serves both.
    fn same_work<'a>(
        &'a self,
        brief: &'a str,
        existing: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<usize>> + Send + 'a>>;

    /// Whether `brief` will touch sensitive data (secrets, credentials,
    /// environment or infrastructure configuration): such a child runs on the
    /// session's own model, not a cheaper subagent model.
    fn restricted<'a>(
        &'a self,
        brief: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>>;
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

    /// The thinking level children run at when their spawn names none
    /// (`subagentDefaultThinking`); `None` inherits the parent's.
    fn default_thinking(&self) -> Option<harness_providers::ThinkingLevel> {
        None
    }

    /// prime-agent's `getRlmSearchableModels`: every catalog model whose
    /// provider has a credential a child can use, for `rlm.find_models`.
    fn searchable(&self) -> Vec<SearchableModel> {
        Vec::new()
    }

    /// [`Self::resolve`] at a thinking level. A level the spawn asked for
    /// (`strict`) that the model does not offer fails it, as prime-agent's
    /// `rlm.spawn(thinking=...)` does; a default one is clamped.
    ///
    /// # Errors
    /// The model cannot be used, or does not offer the level asked for.
    fn resolve_at(
        &self,
        reference: &str,
        _level: Option<harness_providers::ThinkingLevel>,
        _strict: bool,
    ) -> Result<ChildModel, String> {
        self.resolve(reference)
    }
}

/// What a turn gives the children it starts.
/// The session setting a delegated child's task carries (its role): the task is
/// the parent's work, so `/resume` does not offer it as a conversation.
pub const DELEGATED_CHILD_SETTING: &str = "delegated_child";

/// How the app frames a child's first input, prime-agent's `[task from parent]`.
pub const CHILD_PROMPT_HEAD: &str = "[task from parent]";

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
    /// Reference knowledge stays anchored to the parent's original project,
    /// including when a coder and its descendants work in isolated checkouts.
    pub knowledge: Option<super::knowledge::KnowledgeHost>,
    /// The root's memory, read-only: a child recalls what the root kept in its
    /// conversation and globally, and only the root keeps memory.
    pub memory: Option<super::memory::MemoryHost>,
    /// Which models edit by hashline anchors; a child's own model decides.
    pub hashline: super::config::HashlineRule,
    /// The project's `[verify]` checks: `integrate_children` runs them on the
    /// merged revision of its coders before anything reaches the user.
    pub checks: Vec<super::verify::Check>,
    /// What the parent knows that a child must too: the conversation's harness
    /// memory and the skills ha learned from this user. A child (a verifier
    /// most visibly) was sent its brief alone, so a lesson the parent had just
    /// learned - "python is not on PATH here, run the tests in the kernel" - was
    /// unknown to it, and it spent its turn retrying what the lesson ruled out.
    pub context: Vec<harness_session::ContextBlock>,
    /// The root's harness memory, sent to each child as a digest ranked for
    /// its brief; `None` while harness refinements are off.
    pub digest: Option<super::harness::DigestSource>,
    /// What the child's model took from the session's defaults; `None` for
    /// a parent's own launch and for a spawn that named both.
    pub follows: Option<FollowedDefaults>,
}

impl ChildLaunch {
    /// This launch on the model a child asked for: the requested one, else the
    /// configured default, else the parent's. A model that cannot be used fails
    /// the spawn rather than running the child on something else.
    fn for_model(&self, requested: Option<&str>) -> Result<Self, String> {
        self.for_request(requested, None)
    }

    /// The launch of a child in `role`: a verifier or a reviewer judges the
    /// parent's work, so with no model named it runs on the parent's model,
    /// not the children's default (a cheaper model, or one out of quota,
    /// judged the work of a stronger one).
    fn for_role(&self, role: AgentRole, requested: Option<&str>) -> Result<Self, String> {
        let parent = matches!(role, AgentRole::Verifier | AgentRole::Reviewer)
            .then_some(self.model.reference.as_str());
        self.for_model(requested.or(parent))
    }

    /// This launch on the model and thinking level a child asked for, as
    /// prime-agent's `rlm.spawn(model=..., thinking=...)`: the thinking level
    /// asked for, else `subagentDefaultThinking`, else the parent's.
    fn for_request(&self, requested: Option<&str>, thinking: Option<&str>) -> Result<Self, String> {
        let requested = requested
            .map(str::trim)
            .filter(|reference| !reference.is_empty());
        let thinking = thinking
            .map(str::trim)
            .filter(|thinking| !thinking.is_empty());
        let model = resolve_child_model(&self.model, self.models.as_ref(), requested, thinking)?;
        let follows = (requested.is_none() || thinking.is_none()).then(|| FollowedDefaults {
            parent: self.model.clone(),
            model: requested.map(str::to_owned),
            thinking: thinking.map(str::to_owned),
        });
        Ok(Self {
            model,
            follows,
            ..self.clone()
        })
    }
}

/// The model a child runs on: the one asked for, else the configured default,
/// else its parent's; at the thinking level asked for, else
/// `subagentDefaultThinking`, else its parent's.
fn resolve_child_model(
    parent: &ChildModel,
    models: Option<&Arc<dyn ChildModels>>,
    requested: Option<&str>,
    thinking: Option<&str>,
) -> Result<ChildModel, String> {
    let explicit = thinking
        .map(|thinking| {
            harness_providers::ThinkingLevel::parse(thinking).ok_or_else(|| {
                format!(
                    "rlm.spawn thinking must be one of: off, minimal, low, medium, high, xhigh, max (got {thinking:?})"
                )
            })
        })
        .transpose()?;
    let level = explicit.or_else(|| models.and_then(|models| models.default_thinking()));
    let reference = requested
        .map(str::to_owned)
        .or_else(|| models.and_then(|models| models.default_model()));
    if level.is_none()
        && reference
            .as_ref()
            .is_none_or(|reference| reference.eq_ignore_ascii_case(&parent.reference))
    {
        return Ok(parent.clone());
    }
    let reference = reference.unwrap_or_else(|| parent.reference.clone());
    let models = models
        .ok_or_else(|| format!("Requested subagent model \"{reference}\" is not available"))?;
    models.resolve_at(&reference, level, explicit.is_some())
}

/// How a child stands.
#[derive(Clone, Debug)]
enum ChildState {
    Running,
    Done {
        answer: String,
        detail: Value,
    },
    Failed {
        error: String,
    },
    /// A later turn failed, but this earlier terminal answer is still useful.
    /// It is not a successful completion of the delegated task.
    Partial {
        answer: String,
        detail: Value,
        error: String,
    },
    Cancelled {
        reason: String,
    },
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
    /// The thinking level it runs at, when known.
    thinking: Option<String>,
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
    /// The child's task was answered or announced once: a run a later message
    /// wakes it for ends without another no-reply notice, as prime-agent's
    /// `notice_delivered`. Each such notice woke the parent, which messaged
    /// the child again: a verifier waiting on CI and its parent pinged each
    /// other every half minute for hours.
    notice_delivered: bool,
    session_id: Option<SessionId>,
    /// Messages sent before the child's run opened its inbox.
    backlog: Vec<String>,
    notes: VecDeque<String>,
    last_note: Option<Instant>,
    activity: String,
    /// The tool the child is running, while it runs one (prime-agent's
    /// `activity.tool_name`).
    activity_tool: Option<String>,
    /// Where the whole answer was saved when it was longer than a notice holds.
    answer_file: Option<PathBuf>,
    tool_calls: u32,
    cost: String,
    /// The child that spawned this one; `None` for the root agent's children.
    parent: Option<TaskId>,
    /// prime-agent's `rlmDepth`: 1 for the root's children.
    depth: u32,
    /// The `RLM_MAX_DEPTH` fixed when this child was created, as prime-agent
    /// hands a child its parent's value.
    max_depth: u32,
    /// The child is inside a turn: a message reaches it through its run's inbox.
    in_turn: bool,
    /// Sends that selected this turn's inbox and have not finished enqueueing.
    in_flight_messages: usize,
    /// Messages and its own children's notices that arrived between its turns;
    /// its next turn opens with them.
    pending: Vec<String>,
    /// What a follow-up run needs: the brief, the run, its worktree, the
    /// generation the next dispatch takes.
    brief: Option<TaskBrief>,
    run_id: Option<AgentRunId>,
    worktree: Option<WorktreeRecord>,
    generation: u32,
    /// The child's conversation so far, for the turn a message wakes it for.
    history: Vec<ProviderMessage>,
    /// The message the next (follow-up) run opens with.
    wake: Option<String>,
    /// The task the child was given, for a brief rebuilt after a restart.
    objective: String,
    /// The commit a coder's work reached the user's checkout in, once
    /// `integrate_children` applied it.
    integrated: Option<String>,
}

/// One child in the spawn ledger.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct LedgerEntry {
    task_id: String,
    name: String,
    role: String,
    model: String,
    /// The thinking level it ran at, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    depth: u32,
    max_depth: u32,
    parent: Option<String>,
    /// `running`, `completed`, `partial`, `error` or `cancelled`.
    status: String,
    answer: Option<String>,
    error: Option<String>,
    duration_ms: Option<i64>,
    tool_calls: u32,
    objective: String,
    /// The conversation, as `(role, text)`.
    history: Vec<(String, String)>,
    /// A coder's worktree: it stays on disk when the worker stops, so a
    /// coder cut off by a restart is woken in it and carries on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree: Option<WorktreeRecord>,
    /// The commit a coder's work reached the user's checkout in, so a
    /// restart never offers it to `integrate_children` again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    integrated: Option<String>,
}

/// How many messages of a child's conversation the ledger keeps.
const LEDGER_HISTORY: usize = 40;

impl ChildRecord {
    fn ledger_entry(&self) -> LedgerEntry {
        let (status, answer, error) = match &self.state {
            ChildState::Running => ("running", None, None),
            ChildState::Done { answer, .. } => ("completed", Some(answer.clone()), None),
            ChildState::Partial { answer, error, .. } => {
                ("partial", Some(answer.clone()), Some(error.clone()))
            }
            ChildState::Failed { error } => ("error", None, Some(error.clone())),
            ChildState::Cancelled { reason } => ("cancelled", None, Some(reason.clone())),
        };
        LedgerEntry {
            task_id: self.task_id.as_str().to_owned(),
            name: self.name.clone(),
            role: self.role.as_str().to_owned(),
            model: self.model.clone(),
            thinking: self.thinking.clone(),
            depth: self.depth,
            max_depth: self.max_depth,
            parent: self
                .parent
                .as_ref()
                .map(|parent| parent.as_str().to_owned()),
            status: status.to_owned(),
            answer,
            error,
            duration_ms: Some(self.elapsed_ms()),
            tool_calls: self.tool_calls,
            objective: self.objective.clone(),
            history: self
                .history
                .iter()
                .skip(self.history.len().saturating_sub(LEDGER_HISTORY))
                .map(|message| {
                    (
                        match message.role {
                            MessageRole::User => "user",
                            _ => "assistant",
                        }
                        .to_owned(),
                        message.content.clone(),
                    )
                })
                .collect(),
            worktree: self.worktree.clone(),
            integrated: self.integrated.clone(),
        }
    }

    /// A child the ledger brings back. One that was running when its worker
    /// went away failed, as prime-agent reports an interrupted child.
    fn from_ledger(entry: LedgerEntry) -> Option<Self> {
        let task_id = TaskId::parse(entry.task_id).ok()?;
        // Every role comes back as itself: a verifier read back as an explorer
        // lost the verifier's brief and grants when it was woken.
        let role = match entry.role.as_str() {
            "coder" => AgentRole::Coder,
            "verifier" => AgentRole::Verifier,
            "reviewer" => AgentRole::Reviewer,
            _ => AgentRole::Explorer,
        };
        let state = match entry.status.as_str() {
            "completed" => ChildState::Done {
                answer: entry.answer.unwrap_or_default(),
                detail: Value::Null,
            },
            "partial" => ChildState::Partial {
                answer: entry.answer.unwrap_or_default(),
                detail: Value::Null,
                error: entry.error.unwrap_or_default(),
            },
            "cancelled" => ChildState::Cancelled {
                reason: entry.error.unwrap_or_default(),
            },
            "running" => ChildState::Failed {
                error: "the child was interrupted: its worker stopped before it finished"
                    .to_owned(),
            },
            _ => ChildState::Failed {
                error: entry.error.unwrap_or_default(),
            },
        };
        let announced = matches!(state, ChildState::Done { .. });
        Some(Self {
            task_id,
            name: entry.name,
            role,
            model: entry.model,
            thinking: entry.thinking,
            started: Instant::now(),
            duration_ms: entry.duration_ms,
            state,
            cancellation: CancellationToken::new(),
            cancel_reason: None,
            waiters: 0,
            deleted: false,
            suppress_notice: true,
            replied: false,
            // A task done before the restart was answered then; one cut off
            // (now failed) still reports when it is woken and carries on.
            notice_delivered: announced,
            session_id: None,
            backlog: Vec::new(),
            notes: VecDeque::new(),
            last_note: None,
            activity: "inactive".to_owned(),
            activity_tool: None,
            answer_file: None,
            tool_calls: entry.tool_calls,
            cost: "n/a".to_owned(),
            parent: entry.parent.and_then(|parent| TaskId::parse(parent).ok()),
            depth: entry.depth,
            max_depth: entry.max_depth,
            in_turn: false,
            in_flight_messages: 0,
            pending: Vec::new(),
            brief: None,
            run_id: None,
            generation: 1,
            history: entry
                .history
                .into_iter()
                .map(|(role, text)| {
                    ProviderMessage::new(
                        if role == "user" {
                            MessageRole::User
                        } else {
                            MessageRole::Assistant
                        },
                        text,
                    )
                })
                .collect(),
            wake: None,
            objective: entry.objective,
            // A worktree the user removed since is not brought back.
            worktree: entry
                .worktree
                .filter(|worktree| std::path::Path::new(&worktree.path).is_dir()),
            integrated: entry.integrated,
        })
    }
    fn elapsed_ms(&self) -> i64 {
        self.duration_ms.unwrap_or_else(|| {
            i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX)
        })
    }

    /// prime-agent's `list_subagents` row.
    fn row(&self, session_dir: &str) -> Value {
        let (status, answer) = match &self.state {
            ChildState::Running => ("running", None),
            ChildState::Done { answer, .. } => {
                ("completed", Some(clip(answer, self.answer_file.as_deref())))
            }
            ChildState::Partial { answer, .. } => {
                ("error", Some(clip(answer, self.answer_file.as_deref())))
            }
            ChildState::Failed { .. } => ("error", None),
            // prime-agent keeps a cancelled child's status verbatim in its row.
            ChildState::Cancelled { .. } => ("cancelled", None),
        };
        json!({
            "rlm_child_id": self.task_id.as_str(),
            "session_id": self.task_id.as_str(),
            "active_session_id": (!self.state.settled())
                .then(|| self.session_id.as_ref().map(SessionId::as_str))
                .flatten(),
            "session_name": self.name,
            "session_dir": session_dir,
            "status": status,
            "model": self.model,
            // prime-agent's `RLMSubagentActivity`: its runtime accepts only
            // these kinds, and refused the whole list for ha's free text.
            "activity": match (&self.state, &self.activity_tool) {
                (ChildState::Running, Some(tool)) => json!({"kind": "executing", "tool_name": tool}),
                (ChildState::Running, None) => json!({"kind": "waiting"}),
                _ => Value::Null,
            },
            "tool_use_count": self.tool_calls,
            "duration_ms": self.elapsed_ms(),
            "answer_preview": answer,
            "replied_since_task": self.replied,
            "progress_note": self.notes.back(),
            "partial": matches!(self.state, ChildState::Partial { .. }),
            "error": match &self.state {
                ChildState::Failed { error } | ChildState::Partial { error, .. } => Some(error),
                ChildState::Cancelled { reason } => Some(reason),
                _ => None,
            },
        })
    }

    fn observation(&self) -> Value {
        let active = !self.state.settled() && self.session_id.is_some();
        json!({
            "sessionId": self.task_id.as_str(),
            "sessionName": self.name,
            "activeSessionId": active.then(|| self.session_id.as_ref().map(SessionId::as_str)).flatten(),
            "relationship": "child",
            "status": if self.state.settled() { "inactive" } else { "running" },
            "isSessionActive": active,
            "activity": match (&self.state, &self.activity_tool) {
                (ChildState::Running, Some(tool)) => json!({"kind": "executing", "tool_name": tool}),
                (ChildState::Running, None) => json!({"kind": "waiting"}),
                _ => Value::Null,
            },
            "model": self.model,
            "toolUseCount": self.tool_calls,
            "messageCount": self.history.len(),
            "latestMessage": self.history.last().map(|message| message_preview(message, 800)),
            "firstMessage": (!active).then(|| json!({"role": "user", "text": self.objective.chars().take(800).collect::<String>()})),
        })
    }

    /// prime-agent's `RLMChildResult`.
    fn result(&self, session_dir: &str) -> Value {
        let (status, answer, error) = match &self.state {
            ChildState::Running => ("running", None, None),
            ChildState::Done { answer, .. } => (
                "done",
                Some(clip(answer, self.answer_file.as_deref())),
                None,
            ),
            ChildState::Partial { answer, error, .. } => (
                "error",
                Some(clip(answer, self.answer_file.as_deref())),
                Some(error.clone()),
            ),
            ChildState::Failed { error } => ("error", None, Some(error.clone())),
            ChildState::Cancelled { reason } => ("cancelled", None, Some(reason.clone())),
        };
        json!({
            "rlm_child_id": self.task_id.as_str(),
            "session_name": self.name,
            "session_dir": session_dir,
            "status": status,
            "settled": self.state.settled(),
            "partial": matches!(self.state, ChildState::Partial { .. }),
            // prime-agent's settle-binding lane: the whole answer (bounded, the
            // rest in its file), beside the roster's preview.
            "answer_text": answer,
            "answer_preview": answer,
            "error": error,
            "duration_ms": self.state.settled().then(|| self.elapsed_ms()),
            "tool_use_count": self.tool_calls,
            "replied_since_task": self.replied,
        })
    }

    /// One line of `/agents`.
    /// The `/agents` panel's columns for this child: status, name, state,
    /// time, tool calls, cost, model; the note, when there is one, last.
    fn columns(&self) -> [String; 8] {
        let (glyph, state) = match &self.state {
            ChildState::Running => ("●", format!("running · {}", self.activity)),
            ChildState::Done { .. } => ("✓", "completed".to_owned()),
            ChildState::Failed { error } => ("✗", format!("failed: {}", first_line(error))),
            ChildState::Partial { error, .. } => (
                "✗",
                format!("failed (partial answer): {}", first_line(error)),
            ),
            ChildState::Cancelled { reason } => ("○", format!("cancelled: {}", first_line(reason))),
        };
        let indent = "  ".repeat(usize::try_from(self.depth.saturating_sub(1)).unwrap_or(0));
        // `verifier-018e27b1` already says it is a verifier.
        let name = if self.name.starts_with(self.role.as_str()) {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.role.as_str())
        };
        let seconds = self.elapsed_ms() / 1000;
        let elapsed = if seconds >= 60 {
            format!("{}m {:02}s", seconds / 60, seconds % 60)
        } else {
            format!("{seconds}s")
        };
        let model = self
            .model
            .rsplit_once('/')
            .map_or(self.model.as_str(), |(_, model)| model)
            .to_owned();
        [
            format!("{indent}{glyph}"),
            name,
            state,
            elapsed,
            format!(
                "{} tool{}",
                self.tool_calls,
                if self.tool_calls == 1 { "" } else { "s" }
            ),
            self.cost.clone(),
            model,
            self.notes
                .back()
                .map(|note| format!("note: {}", first_line(note)))
                .unwrap_or_default(),
        ]
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// prime-agent's terminal notice for a settled child (`core/messages.ts`), or
/// `None` when the child already told its parent what it found.
fn terminal_notice(
    name: &str,
    state: &ChildState,
    replied: bool,
    answer_file: Option<&Path>,
) -> Option<String> {
    match state {
        ChildState::Running => None,
        ChildState::Failed { error } => Some(format!("[child-failed child:{name}]\n\n{error}")),
        ChildState::Partial { answer, error, .. } => Some(format!(
            "[child-failed child:{name}]\n\n{error}\n\nLast verified answer (partial): {}",
            clip(answer, answer_file)
        )),
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
                clip(answer, answer_file)
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
    /// The model each running child calls, which a change of the session's
    /// defaults replaces between two of its calls.
    live: Mutex<HashMap<TaskId, Arc<LiveModel>>>,
    /// `[agents] max_workers`: how many of the root's children run at once,
    /// 0 for all of them; the ones running now; a child's end, for those waiting.
    max_workers: std::sync::atomic::AtomicU32,
    /// The jev plugin's judgments of a child before it starts, when on.
    judge: Mutex<Option<Arc<dyn ChildJudge>>>,
    /// The jev plugin's command guard, which children's commands pass too.
    guard: Mutex<Option<Arc<dyn harness_tools::ToolGuard>>>,
    /// The jev plugin's judges of a child's own turn: what a long result leaves
    /// out, what is shortened first as the turn grows, and whether its answer
    /// claims an unchecked change works.
    results: Mutex<Option<TurnJudges>>,
    running_workers: Mutex<u32>,
    worker_freed: tokio::sync::Notify,
    children: Mutex<Vec<ChildRecord>>,
    changed: tokio::sync::Notify,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    /// The session's `RLM_MAX_DEPTH` and where it came from.
    max_depth: Mutex<(u32, MaxDepthSource)>,
    /// `[agents] max_depth` as the config last said it.
    configured_depth: Mutex<Option<u32>>,
    /// The session's children, for a child that spawns children of its own.
    agents: std::sync::OnceLock<std::sync::Weak<SessionAgents>>,
    /// Where `rlm.create_session` starts a top-level agent, when the session
    /// runs in a background worker.
    session_host: Mutex<Option<Arc<dyn super::agents::SessionHost>>>,
    /// Where a child's whole answer is kept when it is too long for a notice.
    answers: std::sync::OnceLock<PathBuf>,
    /// The messages the agents of this session sent each other, oldest first.
    exchanges: Mutex<std::collections::VecDeque<Exchange>>,
    /// Where a child's `ask_user` reaches the user, when the session has a
    /// screen to ask on.
    question_host: Mutex<Option<Arc<dyn harness_tools::QuestionHost>>>,
    /// The spawn ledger of the conversation in use.
    ledger_dir: std::sync::OnceLock<PathBuf>,
    ledger_path: Mutex<Option<PathBuf>>,
    /// The launch of the latest turn: what a woken child that came back from
    /// the ledger runs with.
    default_launch: Mutex<Option<ChildLaunch>>,
    root_identity: Mutex<(Option<String>, Option<String>)>,
    /// What every child of the session spent, deleted ones included, at
    /// every depth (prime-agent d592d5e7): `(dollars, some usage had no price)`.
    spend: Mutex<(f64, bool)>,
}

/// One message between two agents of the family.
struct Exchange {
    at: chrono::DateTime<chrono::Local>,
    from: String,
    to: String,
    text: String,
}

/// A send that selected a live inbox must finish before that turn drains it.
struct ChildMessageDelivery<'a> {
    shared: &'a AgentsShared,
    task_id: &'a TaskId,
}

impl Drop for ChildMessageDelivery<'_> {
    fn drop(&mut self) {
        self.shared.update(self.task_id, |child| {
            child.in_flight_messages = child.in_flight_messages.saturating_sub(1);
        });
        self.shared.changed.notify_waiters();
    }
}

/// How many exchanges `/agents messages` keeps.
const EXCHANGES_KEPT: usize = 500;

/// A running child's place under `[agents] max_workers`, given back when its
/// run ends.
struct WorkerSlot(Arc<AgentsShared>);

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        if let Ok(mut running) = self.0.running_workers.lock() {
            *running = running.saturating_sub(1);
        }
        self.0.worker_freed.notify_waiters();
    }
}

impl AgentsShared {
    fn new(
        store: Arc<SharedStore>,
        sender: UnboundedSender<SessionEvent>,
        approval_gate: Arc<dyn ApprovalGate>,
    ) -> Self {
        Self {
            store,
            sender,
            approval_gate,
            ledger: Mutex::new(None),
            launches: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
            max_workers: std::sync::atomic::AtomicU32::new(0),
            judge: Mutex::new(None),
            guard: Mutex::new(None),
            results: Mutex::new(None),
            running_workers: Mutex::new(0),
            worker_freed: tokio::sync::Notify::new(),
            children: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            buckets: Mutex::new(HashMap::new()),
            exchanges: Mutex::new(std::collections::VecDeque::new()),
            question_host: Mutex::new(None),
            max_depth: Mutex::new((DEFAULT_RLM_MAX_DEPTH, MaxDepthSource::Default)),
            configured_depth: Mutex::new(None),
            agents: std::sync::OnceLock::new(),
            session_host: Mutex::new(None),
            answers: std::sync::OnceLock::new(),
            ledger_dir: std::sync::OnceLock::new(),
            ledger_path: Mutex::new(None),
            default_launch: Mutex::new(None),
            root_identity: Mutex::new((None, None)),
            spend: Mutex::new((0.0, false)),
        }
    }

    /// Count one child response toward the session's spend.
    fn charge(&self, price: Option<ModelPrice>, usage: CostUsage) {
        if let Ok(mut spend) = self.spend.lock() {
            match super::cost::calculate_cost(price, usage) {
                Some(cost) => spend.0 += cost,
                None => spend.1 = true,
            }
        }
    }

    /// Write the spawn ledger of the conversation in use (prime-agent's
    /// spawn ledger): every child that was not deleted.
    fn persist_ledger(&self) {
        let Some(path) = self.ledger_path.lock().ok().and_then(|path| path.clone()) else {
            return;
        };
        let entries: Vec<LedgerEntry> = self.children.lock().map_or_else(
            |_| Vec::new(),
            |children| {
                children
                    .iter()
                    .filter(|child| !child.deleted)
                    .map(ChildRecord::ledger_entry)
                    .collect()
            },
        );
        if entries.is_empty() && !path.exists() {
            return;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(&entries) {
            let staged = path.with_extension("json.staged");
            if std::fs::write(&staged, text).is_ok() {
                let _ = std::fs::rename(&staged, &path);
            }
        }
    }

    fn max_depth(&self) -> (u32, MaxDepthSource) {
        self.max_depth
            .lock()
            .map_or((DEFAULT_RLM_MAX_DEPTH, MaxDepthSource::Default), |value| {
                *value
            })
    }

    /// Stop `task_id`'s running descendants: a child that is gone takes its
    /// children with it, as closing a prime-agent session closes its subtree.
    fn cancel_descendants(&self, task_id: &TaskId, reason: &str) {
        let Ok(mut children) = self.children.lock() else {
            return;
        };
        let mut parents = vec![task_id.clone()];
        while let Some(parent) = parents.pop() {
            for child in children
                .iter_mut()
                .filter(|child| child.parent.as_ref() == Some(&parent))
            {
                parents.push(child.task_id.clone());
                if !child.state.settled() {
                    child.cancel_reason = Some(reason.to_owned());
                    child.cancellation.cancel();
                }
            }
        }
    }

    /// Wait for a place to run under `[agents] max_workers`; with none set
    /// (0) there is always one. Only the root's children count: a coder that
    /// waits for its own explorer would otherwise hold the place it needs.
    async fn worker_slot(self: &Arc<Self>) -> WorkerSlot {
        loop {
            let freed = self.worker_freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            {
                let Ok(mut running) = self.running_workers.lock() else {
                    return WorkerSlot(Arc::clone(self));
                };
                let max = self.max_workers.load(Ordering::SeqCst);
                if max == 0 || *running < max {
                    *running += 1;
                    return WorkerSlot(Arc::clone(self));
                }
            }
            freed.await;
        }
    }

    fn update<R>(&self, task_id: &TaskId, change: impl FnOnce(&mut ChildRecord) -> R) -> Option<R> {
        let mut children = self.children.lock().ok()?;
        let child = children
            .iter_mut()
            .find(|child| &child.task_id == task_id)?;
        Some(change(child))
    }

    /// Record a settled worker and tell the parent when nobody is waiting for it.
    fn settle(&self, task_id: &TaskId, outcome: WorkerOutcome) {
        let notice =
            self.update(task_id, |child| {
                child.duration_ms = Some(child.elapsed_ms());
                child.state = if child.cancellation.is_cancelled() {
                    ChildState::Cancelled {
                        reason: child.cancel_reason.clone().unwrap_or_default(),
                    }
                } else {
                    outcome_state(child.role, outcome)
                };
                // What the parent model reads as `[child-failed ...]`, kept where
                // a bug report finds it: a child's `writer_locked` was seen only
                // in the parent's answer.
                let failure = match &child.state {
                    ChildState::Failed { error } => Some(("failed", error)),
                    ChildState::Partial { error, .. } => Some(("partial", error)),
                    _ => None,
                };
                if let Some((event, error)) = failure {
                    let mut record = harness_types::diag::warn("child", event)
                        .task(child.task_id.as_str())
                        .msg(error.clone())
                        .field("child", child.name.clone())
                        .field("role", child.role.as_str())
                        .field("duration_ms", child.duration_ms);
                    if let Some(parent) = &child.parent {
                        record = record.field("parent", parent.as_str());
                    }
                    record.emit();
                }
                if let ChildState::Done { detail, .. } | ChildState::Partial { detail, .. } =
                    &child.state
                    && let Some(count) = detail["tool_calls"].as_u64()
                {
                    child.tool_calls = u32::try_from(count).unwrap_or(u32::MAX);
                }
                if let (
                    ChildState::Done { answer, .. } | ChildState::Partial { answer, .. },
                    Some(dir),
                ) = (&child.state, self.answers.get())
                    && answer.chars().count() > RLM_ANSWER_MAX_CHARS
                {
                    child.answer_file = save_answer(dir, &child.task_id, answer);
                }
                // A send can queue after the backend's last quiet check but
                // before this scheduler boundary. Keep that accepted message
                // with a successful child and wake it before announcing idle.
                let pending = !child.deleted
                    && matches!(child.state, ChildState::Done { .. })
                    && !child.pending.is_empty();
                let done = matches!(child.state, ChildState::Done { .. });
                let notice = (!pending
                    && child.waiters == 0
                    && !child.suppress_notice
                    && (!done || !child.notice_delivered))
                    .then(|| {
                        terminal_notice(
                            &child.name,
                            &child.state,
                            child.replied,
                            child.answer_file.as_deref(),
                        )
                    })
                    .flatten();
                // The task's answer reached the parent - by a notice, its own
                // message or a caller waiting for it - once and for all.
                if done && !pending {
                    child.notice_delivered = true;
                }
                // A top-level child's notice is sent while its state turns
                // settled, under the same lock: sent after, a run that saw no
                // child running (`ha exec`'s wait) could gather the notices
                // before this one, and answered without the second child's.
                let notice = match (&child.parent, notice) {
                    (None, Some(notice)) => {
                        let _ = self.sender.send(SessionEvent::ChildSettled {
                            name: child.name.clone(),
                            notice,
                        });
                        None
                    }
                    (_, notice) => notice,
                };
                (
                    child.parent.clone(),
                    notice.map(|notice| (child.name.clone(), notice)),
                    pending,
                )
            });
        self.cancel_descendants(task_id, "its parent agent finished");
        self.persist_ledger();
        match notice {
            // A nested child reports to the child that spawned it, which reads
            // the notice when its turn ends (prime-agent's `waitForRlmQuiescence`).
            Some((Some(parent), Some((_, notice)), _)) => {
                self.update(&parent, |parent| parent.pending.push(notice));
            }
            Some((_, _, true)) => {
                // Leave pending attached until wake claims it under the same
                // lock that marks Running. A competing sender's wake must
                // claim these messages too, even if it finishes before here.
                let _ = self
                    .agents
                    .get()
                    .and_then(std::sync::Weak::upgrade)
                    .ok_or_else(|| "the session's agents are gone".to_owned())
                    .and_then(|agents| agents.wake_messages(task_id, None));
            }
            _ => {}
        }
        self.changed.notify_waiters();
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

    /// After a turn of `task_id`: wait until it has something to read - its own
    /// children's notices, messages that arrived between turns - and return it,
    /// or return `None` once none of its children is running and nothing is
    /// waiting (prime-agent's `waitForRlmQuiescence`), or when it is stopped.
    async fn quiet(&self, task_id: &TaskId, cancel: &CancellationToken) -> Option<Vec<String>> {
        loop {
            let changed = self.changed.notified();
            let (pending, open) = {
                let mut children = self.children.lock().ok()?;
                let open = children
                    .iter()
                    .filter(|child| child.parent.as_ref() == Some(task_id))
                    .any(|child| !child.state.settled());
                let pending = children
                    .iter_mut()
                    .find(|child| &child.task_id == task_id)
                    .map(|child| std::mem::take(&mut child.pending))
                    .unwrap_or_default();
                (pending, open)
            };
            if !pending.is_empty() {
                return Some(pending);
            }
            if !open {
                return None;
            }
            tokio::select! {
                () = changed => {}
                () = cancel.cancelled() => return None,
            }
        }
    }

    /// Keep a delivered message and show it: the user sees the agents talk.
    fn record(&self, from: &str, to: &str, text: &str) {
        if let Ok(mut exchanges) = self.exchanges.lock() {
            if exchanges.len() == EXCHANGES_KEPT {
                exchanges.pop_front();
            }
            exchanges.push_back(Exchange {
                at: chrono::Local::now(),
                from: from.to_owned(),
                to: to.to_owned(),
                text: text.to_owned(),
            });
        }
        let _ = self.sender.send(SessionEvent::AgentExchange {
            from: from.to_owned(),
            to: to.to_owned(),
            text: text.to_owned(),
        });
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
        enum Route {
            Wake,
            Queued,
            Inbox(SessionId),
        }
        let session = loop {
            let route = self
                .update(task_id, |child| {
                    if child.deleted {
                        return Err("the child was deleted".to_owned());
                    }
                    if child.state.settled() {
                        return Ok(Route::Wake);
                    }
                    match &child.session_id {
                        None => {
                            child.backlog.push(text.clone());
                            Ok(Route::Queued)
                        }
                        Some(_) if !child.in_turn => {
                            child.pending.push(text.clone());
                            Ok(Route::Queued)
                        }
                        Some(session) => {
                            child.in_flight_messages += 1;
                            Ok(Route::Inbox(session.clone()))
                        }
                    }
                })
                .ok_or_else(|| "no such child".to_owned())??;
            match route {
                Route::Inbox(session) => break session,
                Route::Queued => {
                    self.changed.notify_waiters();
                    return Ok("queued");
                }
                Route::Wake => {
                    let agents = self
                        .agents
                        .get()
                        .and_then(std::sync::Weak::upgrade)
                        .ok_or("the session's agents are gone")?;
                    match agents.wake(task_id, text.clone()) {
                        Ok(()) => return Ok("delivered"),
                        // Another accepted message may have just woken it.
                        // Route this one to that run rather than dropping it.
                        Err(error) if error == "the child is still running" => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        };
        let _delivery = ChildMessageDelivery {
            shared: self,
            task_id,
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
        Ok("queued")
    }

    /// The children of `parent` (`None`: the root agent's own children).
    fn names(&self, parent: Option<&TaskId>) -> Vec<(TaskId, String, bool)> {
        self.children.lock().map_or_else(
            |_| Vec::new(),
            |children| {
                children
                    .iter()
                    .filter(|child| !child.deleted && child.parent.as_ref() == parent)
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

    fn find(&self, parent: Option<&TaskId>, selector: &str) -> Option<TaskId> {
        if selector.is_empty() {
            return None;
        }
        let children = self.children.lock().ok()?;
        let family = || {
            children
                .iter()
                .filter(move |child| !child.deleted && child.parent.as_ref() == parent)
        };
        if let Some(child) = family().find(|child| {
            child.task_id.as_str() == selector
                || child.name == selector
                || child
                    .session_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == selector)
        }) {
            return Some(child.task_id.clone());
        }
        let mut matches = family().filter(|child| {
            child.task_id.as_str().starts_with(selector)
                || child.task_id.as_str().ends_with(selector)
                || child.session_id.as_ref().is_some_and(|id| {
                    id.as_str().starts_with(selector) || id.as_str().ends_with(selector)
                })
        });
        let found = matches.next()?;
        matches.next().is_none().then(|| found.task_id.clone())
    }

    /// A message from the parent (the root agent) to its children.
    async fn send_from_parent(&self, request: &Value) -> Result<Value, String> {
        let (message, role, name) = message_arguments(request)?;
        self.take_token("parent")?;
        let targets: Vec<(TaskId, String)> = match role.as_str() {
            "child" => {
                let name = name.ok_or("receiver_name is required for a child")?;
                let task_id = self
                    .find(None, &name)
                    .ok_or_else(|| format!("no child named {name:?}"))?;
                vec![(task_id, name)]
            }
            "all" => self
                .names(None)
                .into_iter()
                .map(|(task_id, name, _)| (task_id, name))
                .collect(),
            _ => return Err(AGENT_FAMILY_REACH_ERROR.to_owned()),
        };
        let mut receipts = Vec::new();
        for (task_id, name) in targets {
            let text = format!("[agent-message from parent:root]\n\n{message}");
            receipts.push(match self.deliver_to_child(&task_id, text).await {
                Ok(status) => {
                    self.record("main", &name, &message);
                    receipt(&name, "root", &message, status)
                }
                Err(error) => json!({"target": name, "error": error}),
            });
        }
        Ok(json!({ "receipts": receipts }))
    }

    /// A message a child sends with its `agent_message` tool.
    async fn send_from_child(&self, from: &TaskId, arguments: &Value) -> Result<Value, String> {
        let (message, role, name) = message_arguments(arguments)?;
        let (sender_name, parent) = self
            .update(from, |child| (child.name.clone(), child.parent.clone()))
            .ok_or("the sending child is gone")?;
        self.take_token(from.as_str())?;
        let parent_name = parent
            .as_ref()
            .and_then(|parent| self.update(parent, |child| child.name.clone()))
            .unwrap_or_else(|| "main".to_owned());
        // prime-agent's reach: the parent, the siblings (the parent's other
        // children) and this child's own children.
        let to_parent = async |text: String| -> Result<&'static str, String> {
            self.update(from, |child| child.replied = true);
            match &parent {
                None => {
                    let _ = self.sender.send(SessionEvent::AgentMessage { text });
                    Ok("delivered")
                }
                Some(parent) => self.deliver_to_child(parent, text).await,
            }
        };
        let mut receipts = Vec::new();
        match role.as_str() {
            "parent" => {
                let status = to_parent(format!(
                    "[agent-message from child:{sender_name}]\n\n{message}"
                ))
                .await?;
                self.record(&sender_name, &parent_name, &message);
                receipts.push(receipt("parent", &sender_name, &message, status));
            }
            "sibling" => {
                let name = name.ok_or("receiver_name is required for a sibling")?;
                let task_id = self
                    .find(parent.as_ref(), &name)
                    .filter(|task_id| task_id != from)
                    .ok_or_else(|| format!("no sibling named {name:?}"))?;
                let text = format!("[agent-message from sibling:{sender_name}]\n\n{message}");
                let status = self.deliver_to_child(&task_id, text).await?;
                self.record(&sender_name, &name, &message);
                receipts.push(receipt(&name, &sender_name, &message, status));
            }
            "child" => {
                let name = name.ok_or("receiver_name is required for a child")?;
                let task_id = self
                    .find(Some(from), &name)
                    .ok_or_else(|| format!("no child named {name:?}"))?;
                let text = format!("[agent-message from parent:{sender_name}]\n\n{message}");
                let status = self.deliver_to_child(&task_id, text).await?;
                self.record(&sender_name, &name, &message);
                receipts.push(receipt(&name, &sender_name, &message, status));
            }
            "all" => {
                let delivery = to_parent(format!(
                    "[agent-message from child:{sender_name}]\n\n{message}"
                ))
                .await;
                receipts.push(match delivery {
                    Ok(status) => {
                        self.record(&sender_name, &parent_name, &message);
                        receipt("parent", &sender_name, &message, status)
                    }
                    Err(error) => json!({"target": "parent", "error": error}),
                });
                let siblings = self
                    .names(parent.as_ref())
                    .into_iter()
                    .map(|entry| (entry, "sibling"));
                let children = self
                    .names(Some(from))
                    .into_iter()
                    .map(|entry| (entry, "parent"));
                for ((task_id, name, _), relation) in siblings.chain(children) {
                    if &task_id == from {
                        continue;
                    }
                    let text =
                        format!("[agent-message from {relation}:{sender_name}]\n\n{message}");
                    receipts.push(match self.deliver_to_child(&task_id, text).await {
                        Ok(status) => {
                            self.record(&sender_name, &name, &message);
                            receipt(&name, &sender_name, &message, status)
                        }
                        Err(error) => json!({"target": name, "error": error}),
                    });
                }
            }
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

/// prime-agent's root sibling delivery: steering delivery to another
/// top-level agent, its sender this agent's name.
fn message_root_sibling(
    host: &dyn super::agents::SessionHost,
    request: &Value,
) -> Result<Value, String> {
    let (message, _, name) = message_arguments(request)?;
    let name = name.ok_or("receiver_name is required for a sibling")?;
    let sender = host.agent_name().unwrap_or_else(|| "root".to_owned());
    let status = host.message_sibling(
        &name,
        &format!("[agent-message from sibling:{sender}]\n\n{message}"),
    )?;
    Ok(json!({ "receipts": [receipt(&name, &sender, &message, &status)] }))
}

fn receipt(target: &str, from: &str, message: &str, status: &str) -> Value {
    let at = harness_runtime::now_unix_ms();
    let mut receipt = json!({
        "id": format!("msg-{}", InputId::generate()),
        "source": "agent_message",
        "target": target,
        "from": from,
        "message": message,
        "deliveryStatus": status,
    });
    if status == "delivered" {
        receipt["deliveryMode"] = json!("steer");
        receipt["deliveredAt"] = json!(at);
    } else if status == "queued" {
        receipt["deliveryMode"] = json!("steer");
        receipt["queuedAt"] = json!(at);
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
        .or_else(|| arguments["target"].as_str())
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
    if report.outcome == DelegatedOutcome::Completed {
        return ChildState::Done {
            answer: report.summary,
            detail: report.detail,
        };
    }
    let error = report.detail["error"].as_str().map_or_else(
        || {
            format!(
                "{}: {} reported {}",
                ErrorCode::ResultIncomplete.as_str(),
                role.as_str(),
                report.outcome.as_str()
            )
        },
        str::to_owned,
    );
    if report.outcome == DelegatedOutcome::Failed
        && report.detail["partial"] == true
        && !report.summary.trim().is_empty()
    {
        ChildState::Partial {
            answer: report.summary,
            detail: report.detail,
            error,
        }
    } else {
        ChildState::Failed { error }
    }
}

/// The `delegate` result for a settled child.
fn state_payload(role: AgentRole, name: &str, state: &ChildState) -> Value {
    match state {
        ChildState::Running => json!({
            "role": role.as_str(), "status": "running", "name": name,
        }),
        ChildState::Done { answer, detail } | ChildState::Partial { answer, detail, .. } => json!({
            "role": role.as_str(),
            "status": if matches!(state, ChildState::Partial { .. }) { "failed" } else { "completed" },
            "partial": matches!(state, ChildState::Partial { .. }),
            "error": match state { ChildState::Partial { error, .. } => Some(error), _ => None },
            "name": name,
            // External tool results expose `text` to the model, not this whole
            // object. Keep the failure/partial warning in that text as well.
            "text": if matches!(state, ChildState::Partial { .. }) {
                terminal_notice(name, state, false, None).unwrap_or_default()
            } else if let Some(branch) = detail["branch"].as_str() {
                format!("{answer}\n\n[coder {name}: its changes are on branch {branch} in its own worktree, not in the user's checkout. Call integrate_children to merge them, run the project checks on the merged revision and bring them in.]")
            } else { answer.clone() },
            "partial_answer": if matches!(state, ChildState::Partial { .. }) { Some(answer) } else { None },
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
            "text": terminal_notice(name, state, false, None).unwrap_or_default(),
            "reason": reason,
        }),
    }
}

/// The children of one interactive session.
pub struct SessionAgents {
    shared: Arc<AgentsShared>,
    /// One scheduler per depth, the root's children first, each made the first
    /// time a child reaches its depth: depth is unlimited by default, so the
    /// levels cannot all be made up front. A child that waits for its own
    /// children holds a slot of its level, never one of theirs, so a full level
    /// cannot starve the level it waits on.
    levels: Mutex<Vec<Level>>,
    /// The root's children's scheduler, `levels[0]`, which holds the ledger.
    first: Arc<WorkerScheduler>,
    backend: Arc<dyn WorkerBackend>,
    workspace_manager: Arc<WorkspaceManager>,
    pump: AtomicBool,
}

/// The scheduler of one depth and the signal that it has work.
struct Level {
    scheduler: Arc<WorkerScheduler>,
    dispatched: Arc<tokio::sync::Notify>,
}

impl SessionAgents {
    /// What the session's children spent so far, deleted ones included;
    /// `None` while there is none, or when some of it had no price.
    #[must_use]
    pub fn children_spend(&self) -> Option<f64> {
        let (dollars, unpriced) = *self.shared.spend.lock().ok()?;
        (!unpriced && dollars > 0.0).then_some(dollars)
    }

    /// # Errors
    /// The worker scheduler cannot be created.
    pub fn new(
        store: Arc<SharedStore>,
        sender: UnboundedSender<SessionEvent>,
        approval_gate: Arc<dyn ApprovalGate>,
        state_root: PathBuf,
    ) -> Result<Arc<Self>, HarnessError> {
        let shared = Arc::new(AgentsShared::new(store, sender, approval_gate));
        let backend: Arc<dyn WorkerBackend> = Arc::new(InteractiveWorkerBackend {
            shared: Arc::clone(&shared),
        });
        Self::assemble(shared, state_root, &backend)
    }

    /// Where children ask the user: their `ask_user` waits for the answer there
    /// instead of ending the child's turn on a question no one sees.
    pub fn set_question_host(&self, host: Arc<dyn harness_tools::QuestionHost>) {
        if let Ok(mut current) = self.shared.question_host.lock() {
            *current = Some(host);
        }
    }

    /// Keep `[agents] max_depth` as the config says it, for `/rlm-max-depth`.
    pub fn set_configured_depth(&self, value: Option<u32>) {
        if let Ok(mut configured) = self.shared.configured_depth.lock() {
            *configured = value;
        }
    }

    #[must_use]
    pub fn configured_depth(&self) -> Option<u32> {
        self.shared
            .configured_depth
            .lock()
            .ok()
            .and_then(|value| *value)
    }

    /// Set the session's `RLM_MAX_DEPTH`; children already running keep the
    /// value they were created with.
    pub fn set_max_depth(&self, value: u32, source: MaxDepthSource) {
        if let Ok(mut max_depth) = self.shared.max_depth.lock() {
            *max_depth = (value, source);
        }
    }

    #[must_use]
    pub fn max_depth(&self) -> (u32, MaxDepthSource) {
        self.shared.max_depth()
    }

    /// Set `[agents] max_workers`, 0 for no limit: how many of the root's
    /// children run at once. A raised limit starts the waiting ones at once.
    pub fn set_max_workers(&self, max_workers: u32) {
        self.shared.max_workers.store(max_workers, Ordering::SeqCst);
        self.shared.worker_freed.notify_waiters();
    }

    /// The jev plugin's judge for children started from here on; `None` turns
    /// it off.
    pub fn set_judge(&self, judge: Option<Arc<dyn ChildJudge>>) {
        if let Ok(mut current) = self.shared.judge.lock() {
            *current = judge;
        }
    }

    /// The command guard children's commands pass, as the root's do.
    pub fn set_guard(&self, guard: Option<Arc<dyn harness_tools::ToolGuard>>) {
        if let Ok(mut current) = self.shared.guard.lock() {
            *current = guard;
        }
    }

    /// The judges of children's turns, as the root's turn is judged.
    pub fn set_turn_judges(&self, judges: Option<TurnJudges>) {
        if let Ok(mut current) = self.shared.results.lock() {
            *current = judges;
        }
    }

    fn judge(&self) -> Option<Arc<dyn ChildJudge>> {
        self.shared
            .judge
            .lock()
            .ok()
            .and_then(|judge| judge.clone())
    }

    /// A sibling that already does (or did) `brief`'s work, when the judge
    /// says so: the design notes' deduplicated subgoals. Only running and
    /// finished children of the same role and parent are candidates.
    async fn same_work(
        &self,
        role: AgentRole,
        brief: &str,
        parent: Option<&TaskId>,
    ) -> Option<(TaskId, String)> {
        let judge = self.judge()?;
        let siblings = self
            .shared
            .children
            .lock()
            .ok()?
            .iter()
            .rev()
            .filter(|child| {
                !child.deleted
                    && child.role == role
                    && child.parent.as_ref() == parent
                    && matches!(child.state, ChildState::Running | ChildState::Done { .. })
            })
            .take(10)
            .map(|child| {
                (
                    child.task_id.clone(),
                    child.name.clone(),
                    child.objective.clone(),
                )
            })
            .collect::<Vec<_>>();
        if siblings.is_empty() {
            return None;
        }
        let briefs = siblings
            .iter()
            .map(|(_, _, objective)| objective.clone())
            .collect::<Vec<_>>();
        let index = judge.same_work(brief, &briefs).await?;
        let (task_id, name, _) = siblings.get(index)?.clone();
        harness_types::diag::info("jev", "subgoal_reused")
            .task(task_id.as_str())
            .field("decision", "subgoals")
            .field("acted", true)
            .field("child", name.clone())
            .emit();
        let _ = self.shared.sender.send(SessionEvent::Notice {
            message: format!(
                "jev: {name} already does this work; its result is used instead of starting a duplicate"
            ),
        });
        Some((task_id, name))
    }

    /// The session's own model for a child whose brief the judge finds
    /// sensitive, when children otherwise run on another (the subagent
    /// default): the design notes' security-aware routing.
    async fn routed_model(&self, launch: &ChildLaunch, brief: &str) -> Option<String> {
        let judge = self.judge()?;
        let default = launch.models.as_ref()?.default_model()?;
        if default == launch.model.reference {
            return None;
        }
        if !judge.restricted(brief).await {
            return None;
        }
        harness_types::diag::info("jev", "routed_to_session_model")
            .field("decision", "routing")
            .field("acted", true)
            .field("model", launch.model.reference.clone())
            .field("instead_of", default.clone())
            .emit();
        let _ = self.shared.sender.send(SessionEvent::Notice {
            message: format!(
                "jev: this subtask touches sensitive data; it runs on {} instead of {default}",
                launch.model.reference
            ),
        });
        Some(launch.model.reference.clone())
    }

    /// Move each running child whose model or thinking level came from the
    /// session's defaults to what the defaults give now, from its next model
    /// call; its row and its later runs follow. A child whose spawn named
    /// both keeps them. Returns each moved child's name and model label.
    ///
    /// `/subagent-model` changed only the next child: the user switched a
    /// quota-bound model away and the children already running stayed on it.
    pub fn follow_defaults(&self) -> Vec<(String, String)> {
        let launches: Vec<(TaskId, ChildLaunch)> = self
            .shared
            .launches
            .lock()
            .map(|launches| {
                launches
                    .iter()
                    .map(|(task_id, launch)| (task_id.clone(), launch.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let mut moved = Vec::new();
        for (task_id, launch) in launches {
            let Some(follows) = &launch.follows else {
                continue;
            };
            // A stopped or finished child follows too: woken by a message, it
            // runs again from its stored launch, and used to wake on the model
            // it was stopped on.
            let kept = self
                .shared
                .update(&task_id, |child| {
                    (!child.deleted).then(|| child.name.clone())
                })
                .flatten();
            let Some(name) = kept else {
                continue;
            };
            // A model the defaults name but cannot reach leaves the child
            // where it is: `/subagent-model` checked the one it saved.
            let Ok(model) = resolve_child_model(
                &follows.parent,
                launch.models.as_ref(),
                follows.model.as_deref(),
                follows.thinking.as_deref(),
            ) else {
                continue;
            };
            if model.reference == launch.model.reference && model.thinking == launch.model.thinking
            {
                continue;
            }
            harness_types::diag::info("child", "model_changed")
                .task(task_id.as_str())
                .field("child", name.clone())
                .field("from", launch.model.reference.clone())
                .field("model", model.reference.clone())
                .field("thinking", model.thinking.clone())
                .emit();
            if let Ok(mut launches) = self.shared.launches.lock()
                && let Some(stored) = launches.get_mut(&task_id)
            {
                stored.model = model.clone();
            }
            if let Some(live) = self
                .shared
                .live
                .lock()
                .ok()
                .and_then(|live| live.get(&task_id).cloned())
            {
                live.set(model.clone());
            }
            self.shared.update(&task_id, |child| {
                child.model.clone_from(&model.reference);
                child.thinking.clone_from(&model.thinking);
            });
            let label = model.thinking.as_ref().map_or_else(
                || model.reference.clone(),
                |thinking| format!("{} {thinking}", model.reference),
            );
            moved.push((name, label));
        }
        if !moved.is_empty() {
            self.shared.persist_ledger();
        }
        moved
    }

    fn scheduler(&self) -> &Arc<WorkerScheduler> {
        &self.first
    }

    /// A scheduler for one depth, sharing the session's workspaces.
    fn level_scheduler(
        backend: &Arc<dyn WorkerBackend>,
        workspace_manager: &Arc<WorkspaceManager>,
    ) -> Result<Arc<WorkerScheduler>, HarnessError> {
        WorkerScheduler::new(
            SchedulerConfig {
                max_concurrent_workers: harness_orchestrator::UNBOUNDED_WORKERS,
                max_depth: harness_orchestrator::UNBOUNDED_DEPTH,
                // Every child runs at once and none is refused, as prime-agent
                // admits every spawn; `[agents] max_workers` limits the root's
                // children in [`AgentsShared::worker_slot`] when the user wants.
                // (Three ran at once here; the rest showed `working` with no call.)
                max_queued_workers: harness_orchestrator::UNBOUNDED_WORKERS,
                // prime-agent bounds a child by its own turn - its steps,
                // tool calls and deadline - and the depth, not by a request
                // pool shared across siblings. A pool of 24 ran out after
                // four or five children had each read a large file, and
                // every later step and spawn of the turn was refused.
                budget: DelegationBudget {
                    max_model_requests: u32::MAX,
                    ..DelegationBudget::default()
                },
            },
            Arc::clone(backend),
            Some(Arc::clone(workspace_manager)),
        )
        .map(Arc::new)
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))
    }

    /// The scheduler of `depth` (1 for the root's children) and its signal,
    /// made the first time a child reaches that depth; its pump starts with it
    /// once the session's pumps run.
    fn level(
        &self,
        depth: u32,
    ) -> Result<(Arc<WorkerScheduler>, Arc<tokio::sync::Notify>), HarnessError> {
        let index = usize::try_from(depth.saturating_sub(1)).unwrap_or(usize::MAX);
        let mut levels = self.levels.lock().map_err(|_| {
            HarnessError::new(
                ErrorCode::ServiceUnavailable,
                "the scheduler levels are unavailable",
            )
        })?;
        while levels.len() <= index {
            let level = Level {
                scheduler: Self::level_scheduler(&self.backend, &self.workspace_manager)?,
                dispatched: Arc::new(tokio::sync::Notify::new()),
            };
            if self.pump.load(Ordering::SeqCst) {
                self.pump_level(&level);
            }
            levels.push(level);
        }
        let level = &levels[index];
        Ok((Arc::clone(&level.scheduler), Arc::clone(&level.dispatched)))
    }

    /// The session's children with a scripted worker in place of the model.
    #[cfg(test)]
    fn with_backend(
        store: Arc<SharedStore>,
        sender: UnboundedSender<SessionEvent>,
        approval_gate: Arc<dyn ApprovalGate>,
        state_root: PathBuf,
        backend: &Arc<dyn WorkerBackend>,
    ) -> Arc<Self> {
        let shared = Arc::new(AgentsShared::new(store, sender, approval_gate));
        Self::assemble(shared, state_root, backend).expect("scheduler")
    }

    fn assemble(
        shared: Arc<AgentsShared>,
        state_root: PathBuf,
        backend: &Arc<dyn WorkerBackend>,
    ) -> Result<Arc<Self>, HarnessError> {
        let _ = shared.answers.set(state_root.join("answers"));
        let _ = shared.ledger_dir.set(state_root.join("ledger"));
        let workspace_manager = Arc::new(WorkspaceManager::new(state_root));
        let first = Self::level_scheduler(backend, &workspace_manager)?;
        if let Ok(mut ledger) = shared.ledger.lock() {
            *ledger = Some(Arc::clone(first.ledger()));
        }
        let agents = Arc::new(Self {
            shared,
            levels: Mutex::new(vec![Level {
                scheduler: Arc::clone(&first),
                dispatched: Arc::new(tokio::sync::Notify::new()),
            }]),
            first,
            backend: Arc::clone(backend),
            workspace_manager,
            pump: AtomicBool::new(false),
        });
        let _ = agents.shared.agents.set(Arc::downgrade(&agents));
        Ok(agents)
    }

    #[must_use]
    pub fn store(&self) -> Arc<SharedStore> {
        Arc::clone(&self.shared.store)
    }

    /// `/agents messages`: every message the agents of this session sent each
    /// other, oldest first, whole.
    #[must_use]
    pub fn exchanges(&self) -> Vec<String> {
        let Ok(exchanges) = self.shared.exchanges.lock() else {
            return Vec::new();
        };
        if exchanges.is_empty() {
            return vec![
                "the agents of this session have not sent each other a message".to_owned(),
            ];
        }
        let mut lines = Vec::new();
        for exchange in exchanges.iter() {
            lines.push(format!(
                "{}  {} -> {}",
                exchange.at.format("%H:%M:%S"),
                exchange.from,
                exchange.to
            ));
            lines.extend(exchange.text.lines().map(|line| format!("    {line}")));
            lines.push(String::new());
        }
        lines
    }

    /// The children that ended without finishing - stopped, failed, or cut
    /// off when their worker went away - as the root agent's turn sees them.
    ///
    /// Seen after a resume: the parent's last answer said a verifier "is
    /// checking and will report", the verifier had been stopped, and the
    /// parent - told nothing - redid the check itself. A notice of a child that
    /// settled in an earlier process is not delivered again, so the state is
    /// said here, with the way to pick a child's work back up.
    #[must_use]
    pub fn unfinished_block(&self) -> Option<String> {
        const SHOWN: usize = 8;
        let lines = self
            .shared
            .children
            .lock()
            .ok()?
            .iter()
            .filter(|child| !child.deleted)
            .filter_map(|child| {
                let (what, why) = match &child.state {
                    ChildState::Cancelled { reason } => ("stopped", reason.as_str()),
                    ChildState::Failed { error } => ("failed", error.as_str()),
                    ChildState::Partial { error, .. } => ("partial", error.as_str()),
                    ChildState::Running | ChildState::Done { .. } => return None,
                };
                Some(format!(
                    "- {} ({}, {what}): {}",
                    child.name,
                    child.role.as_str(),
                    first_line(why).chars().take(160).collect::<String>()
                ))
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            return None;
        }
        let skipped = lines.len().saturating_sub(SHOWN);
        let mut text = String::from(
            "Delegated agents of this conversation that ended without finishing their work. Do not claim one is still running. If its work is still needed, send it a message with the agent_message tool (receiver_name: its name) to resume where it stopped: it keeps its conversation, and a coder keeps its worktree and the work in it, so do not redo that work yourself or delegate it again; otherwise leave it.\n",
        );
        text.push_str(&lines[skipped..].join("\n"));
        if skipped > 0 {
            let _ = write!(text, "\n- and {skipped} earlier ones (`/agents`)");
        }
        Some(text)
    }

    /// `/agents`: one line per child, newest last.
    #[must_use]
    pub fn summary(&self) -> Vec<String> {
        let rows = self.shared.children.lock().map_or_else(
            |_| Vec::new(),
            |children| {
                children
                    .iter()
                    .filter(|child| !child.deleted)
                    .map(ChildRecord::columns)
                    .collect::<Vec<_>>()
            },
        );
        // One table: each column as wide as its widest cell, so the names,
        // states and numbers line up instead of running on after ` · `.
        let mut widths = [0_usize; 8];
        for row in &rows {
            for (width, cell) in widths.iter_mut().zip(row) {
                *width = (*width).max(cell.chars().count());
            }
        }
        let lines = rows
            .iter()
            .map(|row| {
                let mut line = String::new();
                for (index, (cell, width)) in row.iter().zip(widths).enumerate() {
                    if index > 0 {
                        line.push_str("  ");
                    }
                    // Numbers read best right-aligned.
                    let _ = if matches!(index, 3..=5) {
                        write!(line, "{cell:>width$}")
                    } else {
                        write!(line, "{cell:<width$}")
                    };
                }
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            vec![format!(
                "no delegated workers; {} model requests so far",
                self.scheduler().ledger().requests_used()
            )]
        } else {
            lines
        }
    }

    /// Let `rlm.create_session` start top-level agents through `host`.
    pub fn set_session_host(&self, host: Arc<dyn super::agents::SessionHost>) {
        if let Ok(mut slot) = self.shared.session_host.lock() {
            *slot = Some(host);
        }
    }

    /// Reserve the conversation before leaving the current one.
    ///
    /// # Errors
    /// The host reports another owner or its state is unavailable.
    pub fn claim_conversation(&self, task: &str) -> Result<(), String> {
        let host = self
            .shared
            .session_host
            .lock()
            .map_err(|_| "session host is unavailable")?
            .clone();
        host.map_or(Ok(()), |host| host.claim_conversation(task))
    }

    /// How many children are still working.
    #[must_use]
    pub fn running(&self) -> usize {
        self.shared.children.lock().map_or(0, |children| {
            children
                .iter()
                .filter(|child| !child.deleted && !child.state.settled())
                .count()
        })
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
        let mut stopped = Vec::new();
        for child in children.iter_mut().filter(|child| {
            !child.deleted
                && !child.state.settled()
                && (selector == "all"
                    || child.name == selector
                    || child.task_id.as_str() == selector)
        }) {
            child.cancel_reason = Some(reason.to_owned());
            child.cancellation.cancel();
            stopped.push(child.task_id.clone());
        }
        drop(children);
        if stopped.is_empty() && selector != "all" {
            return Err(format!("no running child named {selector:?}"));
        }
        for task_id in &stopped {
            self.shared.cancel_descendants(task_id, reason);
        }
        Ok(stopped.len())
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
                    // The ledger keeps it as the conversation left it.
                    child.state = ChildState::Cancelled {
                        reason: "the conversation was closed".to_owned(),
                    };
                    child.duration_ms = Some(child.elapsed_ms());
                }
            }
        }
        self.shared.persist_ledger();
        if let Ok(mut path) = self.shared.ledger_path.lock() {
            *path = None;
        }
        if let Ok(mut children) = self.shared.children.lock() {
            for child in children.iter_mut() {
                child.deleted = true;
            }
        }
    }

    /// The children as the agents view nests them, the running ones first.
    #[must_use]
    pub fn view_rows(&self) -> Vec<super::agents::protocol::SubagentInfo> {
        let mut rows: Vec<super::agents::protocol::SubagentInfo> =
            self.shared.children.lock().map_or_else(
                |_| Vec::new(),
                |children| {
                    children
                        .iter()
                        .filter(|child| !child.deleted)
                        .map(|child| super::agents::protocol::SubagentInfo {
                            name: child.name.clone(),
                            // A child the user stopped did not fail: it was
                            // drawn as a red "error" beside the ones that broke.
                            status: match child.state {
                                ChildState::Running => "running",
                                ChildState::Done { .. } => "completed",
                                ChildState::Cancelled { .. } => "stopped",
                                _ => "error",
                            }
                            .to_owned(),
                            queued: matches!(child.state, ChildState::Running)
                                && child.activity == "queued",
                            model: child.model.clone(),
                            thinking: child.thinking.clone(),
                            depth: child.depth,
                            // The view shows a program's first line: the
                            // whole of every child's brief, read every
                            // second by every terminal, was the roster's bulk.
                            program: child
                                .objective
                                .lines()
                                .find(|line| !line.trim().is_empty())
                                .unwrap_or_default()
                                .chars()
                                .take(200)
                                .collect(),
                        })
                        .collect()
                },
            );
        rows.sort_by_key(|row| row.status != "running");
        rows
    }

    /// prime-agent's ledger reseed: the conversation `conversation` is the
    /// one in use; its children come back from its spawn ledger, settled.
    pub fn bind_ledger(&self, conversation: &str) {
        if let Ok(mut identity) = self.shared.root_identity.lock() {
            identity.0 = Some(conversation.to_owned());
        }
        let Some(dir) = self.shared.ledger_dir.get() else {
            return;
        };
        let path = dir.join(format!("{conversation}.json"));
        let entries: Vec<LedgerEntry> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if let Ok(mut children) = self.shared.children.lock() {
            for entry in entries {
                if children
                    .iter()
                    .any(|child| !child.deleted && child.task_id.as_str() == entry.task_id)
                {
                    continue;
                }
                if let Some(record) = ChildRecord::from_ledger(entry) {
                    children.push(record);
                }
            }
        }
        if let Ok(mut current) = self.shared.ledger_path.lock() {
            *current = Some(path);
        }
        self.shared.persist_ledger();
    }

    /// Wake a finished child with `text`: a follow-up run of its task that
    /// continues its conversation (prime-agent's revival of an inactive
    /// child). The child reports as it did when it first ran.
    fn wake(self: &Arc<Self>, task_id: &TaskId, text: String) -> Result<(), String> {
        self.wake_messages(task_id, Some(text))
    }

    /// Atomically claim queued follow-ups when reviving a settled child.
    /// With no new text, a competing wake may already have claimed them.
    fn wake_messages(
        self: &Arc<Self>,
        task_id: &TaskId,
        text: Option<String>,
    ) -> Result<(), String> {
        let launch = self
            .shared
            .launches
            .lock()
            .ok()
            .and_then(|launches| launches.get(task_id).cloned())
            .or_else(|| {
                self.shared
                    .default_launch
                    .lock()
                    .ok()
                    .and_then(|launch| launch.clone())
            })
            .ok_or("the child cannot be woken before its parent has run a turn")?;
        let cancellation = CancellationToken::new();
        let Some((depth, brief, run_id, generation, worktree)) = self
            .shared
            .update(task_id, |child| {
                if child.deleted {
                    return Err("the child was deleted".to_owned());
                }
                if !child.state.settled() {
                    return if text.is_none() {
                        Ok(None)
                    } else {
                        Err("the child is still running".to_owned())
                    };
                }
                if text.is_none() && child.pending.is_empty() {
                    return Ok(None);
                }
                let brief = woken_brief(child, task_id, &launch)?;
                child.brief = Some(brief.clone());
                // A coder woken after a restart runs as the run its worktree
                // was issued to: the grant check holds the two together.
                let run_id = child
                    .run_id
                    .clone()
                    .or_else(|| {
                        child
                            .worktree
                            .as_ref()
                            .map(|worktree| worktree.run_id.clone())
                    })
                    .unwrap_or_else(AgentRunId::generate);
                child.run_id = Some(run_id.clone());
                child.generation += 1;
                child.state = ChildState::Running;
                child.started = Instant::now();
                child.duration_ms = None;
                child.cancellation = cancellation.clone();
                child.cancel_reason = None;
                child.suppress_notice = false;
                child.replied = false;
                let mut pending = std::mem::take(&mut child.pending);
                pending.extend(text);
                child.wake = Some(pending.join("\n\n"));
                "queued".clone_into(&mut child.activity);
                Ok(Some((
                    child.depth,
                    brief,
                    run_id,
                    child.generation,
                    child.worktree.clone(),
                )))
            })
            .ok_or("no such child")??
        else {
            return Ok(());
        };
        if let Ok(mut launches) = self.shared.launches.lock() {
            launches.entry(task_id.clone()).or_insert(launch);
        }
        let (scheduler, dispatched) = self.level(depth).map_err(|error| error.to_string())?;
        self.ensure_pump();
        scheduler
            .dispatch(WorkerRequest {
                task_id: task_id.clone(),
                run_id,
                generation: generation.into(),
                depth,
                brief,
                worktree,
                cancellation,
            })
            .map_err(|error| error.to_string())?;
        dispatched.notify_one();
        self.shared.persist_ledger();
        Ok(())
    }

    fn ensure_pump(self: &Arc<Self>) {
        // Under the levels lock, so a level made at the same moment gets its
        // pump exactly once: here, or in [`Self::level`].
        let Ok(levels) = self.levels.lock() else {
            return;
        };
        if self.pump.swap(true, Ordering::SeqCst) {
            return;
        }
        for level in levels.iter() {
            self.pump_level(level);
        }
    }

    /// Settle what one level's scheduler finishes, for the session's life.
    fn pump_level(&self, level: &Level) {
        let scheduler = Arc::clone(&level.scheduler);
        let shared = Arc::clone(&self.shared);
        let dispatched = Arc::clone(&level.dispatched);
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
        parent: Option<&TaskId>,
    ) -> Result<(TaskId, String), HarnessError> {
        // prime-agent's spawn check: an agent at depth d spawns only while
        // d < RLM_MAX_DEPTH, the root reading the session's value and a child
        // the value it was created with.
        let (parent_depth, max_depth) = match parent {
            None => (0, self.shared.max_depth().0),
            Some(parent) => self
                .shared
                .update(parent, |child| (child.depth, child.max_depth))
                .ok_or_else(|| {
                    HarnessError::new(ErrorCode::InvalidPayload, "the spawning child is gone")
                })?,
        };
        if parent_depth >= max_depth {
            return Err(HarnessError::new(
                ErrorCode::DelegationDepthExceeded,
                depth_limit_error(parent_depth, max_depth),
            ));
        }
        let depth = parent_depth + 1;
        // A coder's worktree comes from the user's checkout, which only the root
        // agent hands out; a child's own children are explorers.
        if parent.is_some() && role != AgentRole::Explorer {
            return Err(HarnessError::new(
                ErrorCode::RoleUnavailable,
                "a child agent can spawn explorers only",
            ));
        }
        // The jev plugin: a brief a sibling already serves gets that sibling.
        if let Some(existing) = self.same_work(role, &brief_text, parent).await {
            return Ok(existing);
        }
        let (scheduler, level_dispatched) = self.level(depth)?;
        scheduler
            .require_admission(1)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let task_id = TaskId::generate();
        let name = name.unwrap_or_else(|| {
            let id = task_id.as_str();
            format!("{}-{}", role.as_str(), &id[id.len().saturating_sub(8)..])
        });
        if self.shared.children.lock().is_ok_and(|children| {
            children
                .iter()
                .any(|child| !child.deleted && child.name == name)
        }) {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("a child named {name:?} already exists"),
            ));
        }
        let run_id = AgentRunId::generate();
        let (workspace, base_commit, base_snapshot, worktree) = match role {
            AgentRole::Explorer | AgentRole::Verifier => (
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
                let record = scheduler
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
                max_depth: harness_orchestrator::UNBOUNDED_DEPTH,
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
                thinking: launch.model.thinking.clone(),
                started: Instant::now(),
                duration_ms: None,
                state: ChildState::Running,
                cancellation: cancellation.clone(),
                cancel_reason: None,
                waiters: 0,
                deleted: false,
                suppress_notice: false,
                replied: false,
                notice_delivered: false,
                session_id: None,
                backlog: Vec::new(),
                notes: VecDeque::new(),
                last_note: None,
                activity: "queued".to_owned(),
                activity_tool: None,
                answer_file: None,
                tool_calls: 0,
                cost: "n/a".to_owned(),
                parent: parent.cloned(),
                depth,
                max_depth,
                in_turn: false,
                in_flight_messages: 0,
                pending: Vec::new(),
                brief: Some(brief.clone()),
                run_id: Some(run_id.clone()),
                worktree: worktree.clone(),
                generation: 1,
                history: Vec::new(),
                wake: None,
                objective: brief_text.clone(),
                integrated: None,
            });
        }
        self.shared.persist_ledger();
        if let Ok(mut launches) = self.shared.launches.lock() {
            launches.insert(task_id.clone(), launch.clone());
        }
        self.ensure_pump();
        let dispatched = scheduler.dispatch(WorkerRequest {
            task_id: task_id.clone(),
            run_id,
            generation: 1,
            depth,
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
        level_dispatched.notify_one();
        harness_types::diag::info("child", "started")
            .task(task_id.as_str())
            .field("child", name.clone())
            .field("role", role.as_str())
            .field("depth", depth)
            .field("max_depth", describe_max_depth(max_depth))
            .field("model", launch.model.reference.clone())
            .field("thinking", launch.model.thinking.clone())
            .emit();
        Ok((task_id, name))
    }
}

/// An explorer's brief over `launch`'s workspace: what a child that came back
/// from the spawn ledger is woken with.
/// The brief a woken child runs under. A child that came back from the ledger
/// has none: a read-only child's (explorer, verifier, reviewer) is rebuilt over
/// the current workspace; a coder's worktree does not come back.
fn woken_brief(
    child: &ChildRecord,
    task_id: &TaskId,
    launch: &ChildLaunch,
) -> Result<TaskBrief, String> {
    match child.brief.clone() {
        Some(brief) => Ok(brief),
        None if matches!(
            child.role,
            AgentRole::Explorer | AgentRole::Verifier | AgentRole::Reviewer
        ) =>
        {
            Ok(TaskBrief {
                role: child.role,
                ..explorer_brief(task_id, &child.objective, launch)
            })
        }
        // A coder's worktree outlives its worker: the coder goes on in it,
        // with the brief it was given rebuilt over that worktree.
        None => match &child.worktree {
            Some(worktree) => coder_brief(task_id, &child.objective, worktree),
            None => Err(
                "this coder's worktree is gone (removed, or it ran before ha kept worktrees across restarts); delegate the work to a new coder"
                    .to_owned(),
            ),
        },
    }
}

/// A coder's brief over the worktree it was given.
fn coder_brief(
    task_id: &TaskId,
    objective: &str,
    worktree: &WorktreeRecord,
) -> Result<TaskBrief, String> {
    let workspace = harness_tools::observe_workspace(worktree.project_id.clone(), &worktree.path)
        .map_err(|error| format!("the coder's worktree cannot be observed: {error}"))?;
    Ok(TaskBrief {
        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
        task_id: task_id.clone(),
        title: objective.chars().take(80).collect(),
        objective: objective.to_owned(),
        acceptance_criteria: vec!["return a concise report answering the brief".to_owned()],
        inputs: vec!["current workspace snapshot".to_owned()],
        base_snapshot: worktree.input_fingerprint.as_str().to_owned(),
        base_commit: worktree.base_commit.clone(),
        workspace,
        grants: DelegationGrants {
            // The project the worktree was issued for, which a later session
            // may name differently.
            project_id: worktree.project_id.clone(),
            task_id: task_id.clone(),
            actions: vec![GrantAction::Read, GrantAction::Propose],
            write_scope: worktree.write_scope.clone(),
            edit_workspace: true,
            max_depth: harness_orchestrator::UNBOUNDED_DEPTH,
            budget: DelegationBudget::default(),
        },
        expected_artifacts: Vec::new(),
        deadline_unix_ms: None,
        role: AgentRole::Coder,
    })
}

fn explorer_brief(task_id: &TaskId, objective: &str, launch: &ChildLaunch) -> TaskBrief {
    TaskBrief {
        schema_version: harness_orchestrator::DELEGATION_CONTRACT_VERSION,
        task_id: task_id.clone(),
        title: objective.chars().take(80).collect(),
        objective: objective.to_owned(),
        acceptance_criteria: vec!["return a concise report answering the brief".to_owned()],
        inputs: vec!["current workspace snapshot".to_owned()],
        base_snapshot: launch.workspace.observed_fingerprint.as_str().to_owned(),
        base_commit: launch.workspace.base_commit.clone(),
        workspace: launch.workspace.clone(),
        grants: DelegationGrants {
            project_id: launch.workspace.project_id.clone(),
            task_id: task_id.clone(),
            actions: vec![GrantAction::Read],
            write_scope: Vec::new(),
            edit_workspace: false,
            max_depth: harness_orchestrator::UNBOUNDED_DEPTH,
            budget: DelegationBudget::default(),
        },
        expected_artifacts: Vec::new(),
        deadline_unix_ms: None,
        role: AgentRole::Explorer,
    }
}

impl harness_tools::PathElsewhere for SessionAgents {
    /// A coder's files stay in its worktree until `integrate_children` merges
    /// them: the parent read the paths a running coder reported in its own
    /// checkout, and each read failed with nothing saying where they were.
    fn elsewhere(&self, path: &str) -> Vec<String> {
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Vec::new();
        }
        let Ok(children) = self.shared.children.lock() else {
            return Vec::new();
        };
        children
            .iter()
            .filter(|child| !child.deleted && child.integrated.is_none())
            .filter_map(|child| {
                let worktree = child.worktree.as_ref()?;
                let found = Path::new(&worktree.path).join(relative);
                found.is_file().then(|| {
                    format!(
                        "{} made it in its worktree ({}); it is not in this checkout until integrate_children merges {}'s work",
                        child.name,
                        found.display(),
                        child.name
                    )
                })
            })
            .collect()
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
        if let Ok(mut current) = agents.shared.default_launch.lock() {
            *current = Some(launch.clone());
        }
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

    /// The verifier the harness itself starts on this turn's launch, to judge
    /// work the turn's model says is done.
    #[must_use]
    pub fn verifier(&self) -> VerifierHandle {
        VerifierHandle {
            agents: Arc::clone(&self.agents),
            launch: self.launch.clone(),
            cancellation: self.dispatcher.parent_cancellation.clone(),
        }
    }

    #[cfg(test)]
    fn delegate_tool(&self) -> &DelegateTool {
        &self.dispatcher
    }
}

/// A verifier child the harness starts and waits for: the agent that did the
/// work never grades it.
#[derive(Clone)]
pub struct VerifierHandle {
    agents: Arc<SessionAgents>,
    launch: ChildLaunch,
    cancellation: CancellationToken,
}

impl VerifierHandle {
    /// Run a verifier on `brief` - on `model` when one is named, else the
    /// parent's model - and return its answer.
    ///
    /// # Errors
    /// The verifier could not start, failed, or was stopped.
    pub async fn judge(&self, brief: String, model: Option<&str>) -> Result<String, String> {
        struct Verification<'a>(&'a SessionAgents, &'a TaskId);
        impl Drop for Verification<'_> {
            fn drop(&mut self) {
                let running = self.0.shared.update(self.1, |child| {
                    if child.state.settled() {
                        return false;
                    }
                    child.suppress_notice = true;
                    true
                });
                if running == Some(true) {
                    let _ = self
                        .0
                        .stop(self.1.as_str(), "the verification was canceled");
                }
            }
        }
        if self.cancellation.is_cancelled() {
            return Err("the verification was canceled before it started".to_owned());
        }
        let launch = self.launch.for_role(AgentRole::Verifier, model)?;
        let (task_id, _) = self
            .agents
            .start(AgentRole::Verifier, brief, None, &launch, None)
            .await
            .map_err(|error| error.to_string())?;
        let _verification = Verification(&self.agents, &task_id);
        match self
            .agents
            .shared
            .wait(&task_id, None, Some(&self.cancellation))
            .await
            .map_err(|error| {
                if error.code() == ErrorCode::ProviderCanceled {
                    "the verification was canceled and its child was stopped".to_owned()
                } else {
                    error.to_string()
                }
            })? {
            Some(ChildState::Done { answer, .. }) => Ok(answer),
            Some(ChildState::Partial { error, .. } | ChildState::Failed { error }) => Err(error),
            Some(ChildState::Cancelled { reason }) => Err(reason),
            Some(ChildState::Running) | None => Err("the verifier's record is gone".to_owned()),
        }
    }
}

struct DelegateCatalog;

impl ExternalToolCatalog for DelegateCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "delegate",
                    "description": "Delegate a bounded explorer or an isolated-worktree coder to investigate or implement the brief, or a verifier to check work independently: a verifier starts with a fresh context, cannot edit, runs what shows whether the work holds and ends with VERDICT: PASS or VERDICT: FAIL - use it before you call work done, since the agent that wrote the work is too generous grading it. By default the call waits for the child's answer; with wait=false it returns at once, the child keeps running after this turn, and its result arrives later as a [child-exited ...] or [child-failed ...] message. Do not message a child only to ask how it is doing: its result, or its own agent_message, reaches you, and each message wakes it for another run that sends no further notice.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "role": {"type": "string", "enum": ["explorer", "coder", "verifier"]},
                            "brief": {"type": "string", "minLength": 1, "maxLength": MAX_BRIEF_CHARS},
                            "wait": {"type": "boolean", "description": "Wait for the child's answer (default true)."}
                        },
                        "required": ["role", "brief"],
                        "additionalProperties": false
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": INTEGRATE_TOOL,
                    "description": "Bring finished coder children's work into the user's checkout. Each coder's remaining changes are committed on its branch, the branches are merged in order in a separate integration worktree, and the project's [verify] checks run on the merged revision. The result is `ready` (with the final commit), `conflicted` (with the conflicting files), `checks_failed` (with the failing check and its output) or `refused` (the user's checkout changed since the coders started; nothing is touched). With apply=true a ready result fast-forwards the user's current branch to the final commit - never a reset, never over uncommitted work - and says how to undo it; without it, nothing in the user's checkout changes.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "children": {
                                "type": "array",
                                "items": {"type": "string", "minLength": 1},
                                "maxItems": 16,
                                "description": "Coder children by name or id, merged in this order. Default: every finished coder not yet integrated, oldest first."
                            },
                            "apply": {"type": "boolean", "description": "Fast-forward the user's branch to a ready result (default false)."}
                        },
                        "additionalProperties": false
                    }
                }
            }),
            json!({
                "type": "function",
                "function": {
                    "name": ROOT_MESSAGE_TOOL,
                    "description": "Send a message to one of your delegated agents by name (receiver_role \"child\", the default, with receiver_name) or to all of them (\"all\"). A running child reads it at its next step; a child that finished or was stopped wakes with it and continues its own conversation where it left off, instead of starting over.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "message": {"type": "string", "minLength": 1, "maxLength": MAX_MESSAGE_CHARS},
                            "receiver_role": {"type": "string", "enum": ["child", "all"]},
                            "receiver_name": {"type": "string", "description": "The child's name, as `/agents` and its reports show it."}
                        },
                        "required": ["message"],
                        "additionalProperties": false
                    }
                }
            }),
        ]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        matches!(name, "delegate" | INTEGRATE_TOOL | ROOT_MESSAGE_TOOL).then(|| {
            CodingToolAction::ExternalTool {
                plugin_id: "delegate".to_owned(),
                tool_name: name.to_owned(),
                arguments: arguments.clone(),
                parent_invocation_id: None,
                timeout_ms: 120_000,
            }
        })
    }
}

/// The root agent's tool that brings its coders' work back.
const INTEGRATE_TOOL: &str = "integrate_children";

/// The root agent's tool that messages its children - and wakes a settled
/// one. Without it the root reached them only through the Python kernel, so
/// with the kernel off a stopped child could not be resumed at all.
const ROOT_MESSAGE_TOOL: &str = "agent_message";

/// What `integrate_children` was asked for.
struct IntegrateRequest {
    children: Option<Vec<String>>,
    apply: bool,
}

impl IntegrateRequest {
    fn parse(arguments: &Value) -> Result<Self, HarnessError> {
        let invalid = |message: &str| HarnessError::new(ErrorCode::InvalidPayload, message);
        let object = arguments
            .as_object()
            .ok_or_else(|| invalid("integrate_children arguments must be an object"))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "children" | "apply"))
        {
            return Err(invalid(
                "integrate_children accepts only children and apply",
            ));
        }
        let children = match object.get("children") {
            None | Some(Value::Null) => None,
            Some(Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map(str::trim)
                            .filter(|item| !item.is_empty())
                            .map(str::to_owned)
                            .ok_or_else(|| invalid("each child must be a non-empty name or id"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .filter(|children| !children.is_empty()),
            Some(_) => return Err(invalid("children must be a list of names or ids")),
        };
        let apply = match object.get("apply") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(apply)) => *apply,
            Some(_) => return Err(invalid("apply must be true or false")),
        };
        Ok(Self { children, apply })
    }
}

/// One coder `integrate_children` merges.
struct IntegrationMember {
    task_id: TaskId,
    name: String,
    worktree: WorktreeRecord,
}

/// The `integrate_children` result: the text the model reads and the fields
/// a caller can match on.
fn integration_payload(status: &str, text: String, extra: Value) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("status".to_owned(), Value::String(status.to_owned()));
    payload.insert("text".to_owned(), Value::String(text));
    if let Value::Object(extra) = extra {
        payload.extend(extra);
    }
    Value::Object(payload)
}

/// Replace the task ids an integration report names with the children's names.
fn name_children(text: &str, members: &[IntegrationMember]) -> String {
    members.iter().fold(text.to_owned(), |text, member| {
        text.replace(member.task_id.as_str(), &member.name)
    })
}

/// The project's checks as the integration runs them: the `[verify]` way, in
/// the shell, stopping at the first failure.
async fn run_integration_checks(
    root: PathBuf,
    checks: Vec<super::verify::Check>,
    cancellation: CancellationToken,
) -> Vec<CheckResult> {
    let report = super::verify::run(&root, &checks, &cancellation).await;
    let mut results = report
        .runs
        .iter()
        .map(|run| CheckResult {
            command: format!("{} (`{}`)", run.name, run.command),
            passed: run.passed,
            exit_code: run
                .exit_text
                .strip_prefix("exited ")
                .and_then(|code| code.parse().ok()),
            detail: if run.output.is_empty() {
                run.exit_text.clone()
            } else {
                format!("{}\n{}", run.exit_text, run.output)
            },
        })
        .collect::<Vec<_>>();
    // A check never reached is not a pass.
    for check in checks.iter().skip(report.runs.len()) {
        if report.failure().is_none() {
            results.push(CheckResult {
                command: format!("{} (`{}`)", check.name, check.command),
                passed: false,
                exit_code: None,
                detail: "did not run".to_owned(),
            });
        }
    }
    results
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
            Some("verifier") => AgentRole::Verifier,
            _ => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "delegate role must be explorer, coder or verifier",
                ));
            }
        };
        let brief = object
            .get("brief")
            .and_then(Value::as_str)
            .filter(|brief| !brief.trim().is_empty() && brief.chars().count() <= MAX_BRIEF_CHARS)
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!("delegate brief must contain 1..={MAX_BRIEF_CHARS} characters"),
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
        // The jev plugin: a sensitive brief stays on the session's model; a
        // model it cannot use falls back to the usual choice.
        let routed = self.agents.routed_model(&self.launch, &brief).await;
        let launch = routed
            .as_deref()
            .and_then(|model| self.launch.for_role(role, Some(model)).ok())
            .map_or_else(|| self.launch.for_role(role, None), Ok)
            .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
        let (task_id, name) = self.agents.start(role, brief, None, &launch, None).await?;
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

    /// The coders `integrate_children` merges, in order: the named ones, or
    /// every finished coder of the root agent not yet integrated.
    fn integration_members(
        &self,
        selectors: Option<&[String]>,
    ) -> Result<Vec<IntegrationMember>, HarnessError> {
        let invalid = |message: String| HarnessError::new(ErrorCode::InvalidPayload, message);
        let shared = &self.agents.shared;
        let named = match selectors {
            Some(selectors) => {
                let mut ids = Vec::new();
                for selector in selectors {
                    let id = shared.find(None, selector).ok_or_else(|| {
                        invalid(format!("no child of yours is named {selector:?}"))
                    })?;
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                Some(ids)
            }
            None => None,
        };
        let children = shared
            .children
            .lock()
            .map_err(|_| invalid("the children's records are unavailable".to_owned()))?;
        let members = match named {
            Some(ids) => ids
                .iter()
                .map(|id| {
                    let child = children
                        .iter()
                        .find(|child| &child.task_id == id)
                        .ok_or_else(|| invalid(format!("child {id} is gone")))?;
                    if child.role != AgentRole::Coder {
                        return Err(invalid(format!(
                            "{} is a {}, and only a coder has work to integrate",
                            child.name,
                            child.role.as_str()
                        )));
                    }
                    if !child.state.settled() {
                        return Err(invalid(format!(
                            "coder {} is still running; wait for its result first",
                            child.name
                        )));
                    }
                    let worktree = child.worktree.clone().ok_or_else(|| {
                        invalid(format!(
                            "coder {} has no worktree in this session (it came back from an earlier one)",
                            child.name
                        ))
                    })?;
                    Ok(IntegrationMember {
                        task_id: child.task_id.clone(),
                        name: child.name.clone(),
                        worktree,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            None => children
                .iter()
                .filter(|child| {
                    !child.deleted
                        && child.parent.is_none()
                        && child.role == AgentRole::Coder
                        && matches!(child.state, ChildState::Done { .. })
                        && child.integrated.is_none()
                })
                .filter_map(|child| {
                    Some(IntegrationMember {
                        task_id: child.task_id.clone(),
                        name: child.name.clone(),
                        worktree: child.worktree.clone()?,
                    })
                })
                .collect(),
        };
        if members.is_empty() {
            return Err(invalid(
                "no finished coder child has work to integrate; delegate to a coder first, or name the coders"
                    .to_owned(),
            ));
        }
        Ok(members)
    }

    /// `integrate_children`: merge finished coders' branches in a separate
    /// integration worktree, run the project's checks on the merged revision
    /// and - when asked and the user's checkout has not moved - fast-forward
    /// the user's branch to it. Every outcome is a result the model reads; an
    /// error is left for what could not be attempted at all.
    #[allow(clippy::too_many_lines, reason = "one integration, told in order")]
    async fn integrate(&self, arguments: &Value) -> Result<Value, HarnessError> {
        fn short(commit: &str) -> &str {
            &commit[..commit.len().min(12)]
        }
        let failed = |error: OrchestratorError| HarnessError::new(error.code(), error.to_string());
        let require_active = || {
            if self.parent_cancellation.is_cancelled() {
                Err(HarnessError::new(
                    ErrorCode::ProviderCanceled,
                    "the integration was canceled; nothing was applied",
                ))
            } else {
                Ok(())
            }
        };
        require_active()?;
        let request = IntegrateRequest::parse(arguments)?;
        let members = self.integration_members(request.children.as_deref())?;
        let names = members
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let first = &members[0].worktree;
        if let Some(other) = members.iter().find(|member| {
            member.worktree.base_commit != first.base_commit
                || member.worktree.input_fingerprint != first.input_fingerprint
        }) {
            return Ok(integration_payload(
                "refused",
                format!(
                    "Refused: coders {} and {} started from different states of the user's checkout ({} and {}), so one of them works on an outdated copy. Nothing was merged or applied. Integrate the coders that started from the current state, or delegate the work again.",
                    members[0].name,
                    other.name,
                    short(&first.base_commit),
                    short(&other.worktree.base_commit)
                ),
                json!({ "children": names }),
            ));
        }
        let manager = &self.agents.workspace_manager;
        let user_root = &self.launch.workspace_root;
        let expected = first.input_fingerprint.clone();
        let refused = |reason: &str| {
            integration_payload(
                "refused",
                format!(
                    "Refused: {reason}. The user's checkout was not touched. Ask the user how to proceed: commit or set aside their changes and delegate again, or merge the coders' branches by hand."
                ),
                json!({ "children": names, "reason": reason }),
            )
        };
        if manager.fingerprint(user_root).await.map_err(failed)? != expected {
            return Ok(refused(&format!(
                "the user's checkout changed since coder {} started (a commit, an edit or a new file), so its work no longer applies to what the user has",
                members[0].name
            )));
        }
        let mut candidates = Vec::new();
        for member in &members {
            let title = self
                .agents
                .shared
                .update(&member.task_id, |child| {
                    // What the coder says it did, before the brief it was
                    // given: a brief opens with "You are working on ...",
                    // which told nothing about the commit.
                    let answer = match &child.state {
                        ChildState::Done { answer, .. } => commit_title(answer),
                        _ => None,
                    };
                    answer
                        .or_else(|| commit_title(&child.objective))
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            // The files it changed say what the commit is; what the coder said
            // it did goes in the body.
            let files = changed_files(std::path::Path::new(&member.worktree.path));
            let message = match (files, title.is_empty()) {
                (Some(files), true) => format!("coder {}: {files}", member.name),
                (Some(files), false) => format!(
                    "coder {}: {files}

{title}",
                    member.name
                ),
                (None, _) => format!("coder {}: {title}", member.name),
            };
            match manager
                .commit_worker_changes(&member.worktree, &message)
                .await
            {
                // Nothing left uncommitted: the coder committed its work itself.
                Err(error) if error.code() == ErrorCode::ResultIncomplete => {}
                Err(error) => return Err(failed(error)),
                Ok(_) => {}
            }
            let revision = manager
                .head_revision(&member.worktree.path)
                .await
                .map_err(failed)?;
            let changes = manager
                .branch_changes(&member.worktree.path, &member.worktree.base_commit)
                .await
                .map_err(failed)?;
            candidates.push(harness_orchestrator::workspace_candidate(
                &member.worktree,
                &revision,
                changes.paths,
            ));
        }
        if candidates
            .iter()
            .all(|candidate| candidate.files.is_empty())
        {
            return Ok(integration_payload(
                "empty",
                format!("Nothing to integrate: {names} changed no file in their worktrees."),
                json!({ "children": names }),
            ));
        }
        let snapshot = VerifiedSnapshot {
            project_id: first.project_id.clone(),
            root: user_root.to_string_lossy().into_owned(),
            base_commit: first.base_commit.clone(),
            base_branch: first.base_branch.clone(),
            fingerprint: expected.clone(),
        };
        let order = members
            .iter()
            .map(|member| member.task_id.clone())
            .collect::<Vec<_>>();
        let integrator = ResultIntegrator::new(Arc::clone(manager));
        let checks = self.launch.checks.clone();
        let cancellation = self.parent_cancellation.clone();
        let outcome = integrator
            .integrate_with(&snapshot, &order, &candidates, |root| {
                run_integration_checks(root, checks, cancellation)
            })
            .await
            .map_err(failed)?;
        let report = match outcome {
            IntegrationOutcome::Conflicted { conflicts, report } => {
                let conflicts = conflicts
                    .iter()
                    .map(|conflict| name_children(conflict, &members))
                    .collect::<Vec<_>>();
                return Ok(integration_payload(
                    "conflicted",
                    format!(
                        "Conflicted: the coders' branches do not merge cleanly.\n{}\nNothing was applied and the user's checkout was not touched. {} of {} branch(es) merged before the conflict. Resolve it by delegating one coder to redo its change on top of the other's, or by narrowing the coders to separate files.",
                        conflicts
                            .iter()
                            .map(|conflict| format!("- {conflict}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        report.steps.len(),
                        members.len()
                    ),
                    json!({ "children": names, "conflicts": conflicts }),
                ));
            }
            IntegrationOutcome::ChecksFailed { failures, report } => {
                return Ok(integration_payload(
                    "checks_failed",
                    format!(
                        "Checks failed on the merged revision {} of {names} (in the integration worktree {}):\n{}\nNothing was applied and the user's checkout was not touched. Fix the failure through a coder and integrate again; do not weaken or skip the check.",
                        short(&report.final_commit),
                        report.integration_root,
                        failures
                            .iter()
                            .map(|failure| format!("- {failure}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                    json!({
                        "children": names,
                        "final_commit": report.final_commit,
                        "failures": failures,
                    }),
                ));
            }
            IntegrationOutcome::Ready(report) => report,
        };
        require_active()?;
        let checks_line = if report.checks.is_empty() {
            "No [verify] checks are configured, so the merged revision was not checked.".to_owned()
        } else {
            format!(
                "The project checks passed on it: {}.",
                report
                    .checks
                    .iter()
                    .map(|check| check.command.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        if !request.apply {
            return Ok(
                match integrator
                    .recheck_before_apply(user_root, &expected, &report)
                    .await
                    .map_err(failed)?
                {
                    FinalApply::Allowed { final_commit } => integration_payload(
                        "ready",
                        format!(
                            "Ready: {names} merged cleanly into {final_commit} in the integration worktree {}. {checks_line} The user's checkout is unchanged. Call integrate_children again with apply=true to fast-forward the user's branch to it, or the user can do it with `git fetch \"{}\" HEAD` and then `git merge --ff-only {final_commit}`.",
                            report.integration_root, report.integration_root
                        ),
                        json!({
                            "children": names,
                            "final_commit": final_commit,
                            "integration_root": report.integration_root,
                        }),
                    ),
                    FinalApply::Refused { reason } => refused(&reason),
                },
            );
        }
        Ok(
            match integrator
                .apply_final(user_root, &expected, &report)
                .await
                .map_err(failed)?
            {
                ApplyOutcome::Applied {
                    branch,
                    previous_head,
                    final_commit,
                } => {
                    for member in &members {
                        self.agents.shared.update(&member.task_id, |child| {
                            child.integrated = Some(final_commit.clone());
                        });
                    }
                    integration_payload(
                        "applied",
                        format!(
                            "Applied: {names} merged into {final_commit}, and the user's branch {branch} was fast-forwarded to it from {}. {checks_line} To undo, the user can run `git reset --keep {previous_head}`.",
                            short(&previous_head)
                        ),
                        json!({
                            "children": names,
                            "final_commit": final_commit,
                            "previous_head": previous_head,
                            "branch": branch,
                        }),
                    )
                }
                ApplyOutcome::Refused { reason } => refused(&reason),
            },
        )
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
            if plugin_id == "delegate" && tool_name == INTEGRATE_TOOL {
                return IntegrateRequest::parse(arguments).map(|_| ());
            }
            if plugin_id == "delegate" && tool_name == ROOT_MESSAGE_TOOL {
                return root_message_request(arguments)
                    .map(|_| ())
                    .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error));
            }
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
                .scheduler()
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
            let payload = match (plugin_id, tool_name) {
                ("delegate", "delegate") => self.run(arguments).await?,
                ("delegate", INTEGRATE_TOOL) => self.integrate(arguments).await?,
                ("delegate", ROOT_MESSAGE_TOOL) => {
                    let request = root_message_request(arguments)
                        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
                    self.agents
                        .shared
                        .send_from_parent(&request)
                        .await
                        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?
                }
                _ => {
                    return Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        "delegation target is unavailable",
                    ));
                }
            };
            Ok(ToolOutput::ExternalTool {
                plugin_id: "delegate".to_owned(),
                tool_name: tool_name.to_owned(),
                payload,
                inflight: 1,
            })
        })
    }
}

/// The root's `agent_message` arguments as [`AgentsShared::send_from_parent`]
/// reads them: the receiver defaults to the named child.
fn root_message_request(arguments: &Value) -> Result<Value, String> {
    let (message, role, name) = message_arguments(arguments)?;
    let role = match (arguments["receiver_role"].as_str(), &name) {
        (None, _) => "child",
        (Some("child" | "all"), _) => role.as_str(),
        (Some(other), _) => {
            return Err(format!(
                "the root agent messages its children (\"child\" or \"all\"), not {other:?}"
            ));
        }
    };
    if role == "child" && name.is_none() {
        return Err("receiver_name names the child to message".to_owned());
    }
    Ok(json!({"message": message, "receiver_role": role, "receiver_name": name}))
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

/// The paths a dirty refusal names before it counts the rest.
const DIRTY_NAMED: usize = 5;

/// Why the checkout cannot seed a coder, and what to do about it.
///
/// Seen: a refusal that listed all thirty-three uncommitted files filled the
/// screen and said nothing about the way out. A coder starts from the last
/// commit, so it would not see uncommitted work and its merge could not
/// carry it; the model is told to commit first (with the user's leave) or to
/// do the edit itself.
fn dirty_reasons(reasons: &[DirtyReason]) -> String {
    let changes = reasons
        .iter()
        .filter(|reason| {
            matches!(
                reason,
                DirtyReason::TrackedModification { .. }
                    | DirtyReason::StagedChange { .. }
                    | DirtyReason::UntrackedFile { .. }
            )
        })
        .collect::<Vec<_>>();
    let mut parts = reasons
        .iter()
        .filter(|reason| {
            matches!(
                reason,
                DirtyReason::DetachedHead | DirtyReason::NotARepository
            )
        })
        .map(DirtyReason::describe)
        .collect::<Vec<_>>();
    if !changes.is_empty() {
        let mut named = changes
            .iter()
            .take(DIRTY_NAMED)
            .map(|reason| reason.describe())
            .collect::<Vec<_>>()
            .join(", ");
        if changes.len() > DIRTY_NAMED {
            let _ = write!(named, ", and {} more", changes.len() - DIRTY_NAMED);
        }
        parts.push(format!(
            "the checkout has {} uncommitted change(s) ({named}). A coder starts from the last commit, so it would not see them; commit them first (ask the user), or make this edit yourself or with an explorer",
            changes.len()
        ));
    }
    parts.join("; ")
}

/// What a coder's worktree changed, for its commit title: up to three paths
/// and how many more, or `None` when Git cannot say.
fn changed_files(worktree: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain=v1", "--untracked-files=all"])
        .current_dir(worktree)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let paths = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.get(3..))
        .map(|path| {
            path.rsplit(" -> ")
                .next()
                .unwrap_or(path)
                .trim_matches('"')
                .to_owned()
        })
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return None;
    }
    let mut title = paths.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > 3 {
        let _ = write!(title, " and {} more", paths.len() - 3);
    }
    Some(title)
}

/// A commit title from a coder's text: its first line with words, without
/// Markdown heading or emphasis marks, at most 72 characters.
fn commit_title(text: &str) -> Option<String> {
    let line = text
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches(['#', '*', '-', '>', ' '])
                .trim()
        })
        .find(|line| line.chars().any(char::is_alphanumeric))?;
    let line = line.replace("**", "").replace('`', "");
    let mut title = line.chars().take(72).collect::<String>();
    if line.chars().count() > 72 {
        title.push('…');
    }
    Some(title)
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

/// A child works under its parent's policy - the permission mode, the allow and
/// deny rules and what the user allowed for this turn - with its parent's tools,
/// as prime-agent's children inherit their parent's permissions and tool set.
/// An explorer does it in its parent's workspace, a coder in its own worktree.
fn child_policy(parent: &ToolPolicy) -> ToolPolicy {
    parent.clone()
}

/// The tools a verifier is neither offered nor allowed: it reads and runs, and
/// what it judges stays as the agent that did the work left it.
const VERIFIER_DENIED_TOOLS: [&str; 3] = ["apply_patch", "write_file", "edit_file"];

/// The parent's policy with every editing tool denied, whatever the mode.
fn verifier_policy(parent: &ToolPolicy) -> ToolPolicy {
    let mut rules = parent.tool_rules();
    rules.extend(VERIFIER_DENIED_TOOLS.iter().map(|tool| {
        harness_tools::ToolPatternRule::deny(
            format!("{tool}(*)"),
            "a verifier does not edit the work it checks",
        )
    }));
    parent.clone().with_tool_rules(rules)
}

/// The coding tools without the editing ones.
fn verifier_schemas() -> Vec<Value> {
    coding_tool_schemas()
        .into_iter()
        .filter(|schema| {
            schema["function"]["name"]
                .as_str()
                .is_none_or(|name| !VERIFIER_DENIED_TOOLS.contains(&name))
        })
        .collect()
}

/// The tools a child has beyond the coding tools: the parent's web tools, and
/// prime-agent's `agent_message` and `rlm.progress_note`, which a prime-agent
/// child calls from its kernel and a child here calls as tools.
struct ChildTools {
    shared: Arc<AgentsShared>,
    task_id: TaskId,
    web: Option<ChildWebTools>,
    knowledge: Option<super::knowledge::KnowledgeHost>,
    memory: Option<super::memory::MemoryHost>,
    /// How this child starts children of its own; `None` at the depth limit.
    spawn: Option<ChildSpawn>,
}

/// What a child that may spawn gives its own children.
struct ChildSpawn {
    launch: ChildLaunch,
    /// The child's own cancellation: waiting for a child of its own stops with it.
    cancellation: CancellationToken,
}

impl ChildTools {
    fn delegate_schema() -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "delegate",
                "description": "Start an explorer child of your own for part of your task. By default the call waits for its answer; with wait=false it returns at once and the result arrives later as a [child-exited ...] or [child-failed ...] message - you are not finished until your children are.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "role": {"type": "string", "enum": ["explorer"]},
                        "brief": {"type": "string", "minLength": 1, "maxLength": MAX_BRIEF_CHARS},
                        "wait": {"type": "boolean", "description": "Wait for the child's answer (default true)."}
                    },
                    "required": ["role", "brief"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Start a child of this child and, unless told not to, wait for it.
    async fn delegate(&self, arguments: &Value) -> Result<Value, HarnessError> {
        let spawn = self.spawn.as_ref().ok_or_else(|| {
            let (depth, max_depth) = self
                .shared
                .update(&self.task_id, |child| (child.depth, child.max_depth))
                .unwrap_or((1, DEFAULT_RLM_MAX_DEPTH));
            HarnessError::new(
                ErrorCode::DelegationDepthExceeded,
                depth_limit_error(depth, max_depth),
            )
        })?;
        let (role, brief, wait) = DelegateTool::arguments(arguments)?;
        let agents = self
            .shared
            .agents
            .get()
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::ServiceUnavailable,
                    "the session's agents are gone",
                )
            })?;
        let (task_id, name) = agents
            .start(role, brief, None, &spawn.launch, Some(&self.task_id))
            .await?;
        if !wait {
            return Ok(json!({
                "role": role.as_str(),
                "status": "started",
                "name": name,
                "task_id": task_id.as_str(),
                "text": format!("started {} {name}; its result arrives as a message when it finishes", role.as_str()),
            }));
        }
        match self
            .shared
            .wait(&task_id, None, Some(&spawn.cancellation))
            .await?
        {
            Some(state) => Ok(state_payload(role, &name, &state)),
            None => Err(HarnessError::new(
                ErrorCode::ServiceUnavailable,
                "the child's record is gone",
            )),
        }
    }

    fn agent_schemas() -> Vec<Value> {
        vec![
            json!({
                "type": "function",
                "function": {
                    "name": "agent_message",
                    "description": "Send a message to your parent agent (receiver_role \"parent\", the default), to a sibling by name (\"sibling\" with receiver_name), to one of your own children by name (\"child\" with receiver_name), or to all of them (\"all\"). The receiver reads it at its next step, or it wakes the root agent when that agent is idle. Use it to report what you found before you finish. Do not send a child a message only to ask for its status: each one wakes it for another run, and its result or its own message reaches you without asking.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "message": {"type": "string", "minLength": 1, "maxLength": MAX_MESSAGE_CHARS},
                            "receiver_role": {"type": "string", "enum": ["parent", "sibling", "child", "all"]},
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
        if self.spawn.is_some() {
            schemas.push(Self::delegate_schema());
        }
        if let Some((catalog, _)) = &self.web {
            schemas.extend(catalog.schemas());
        }
        if let Some(knowledge) = &self.knowledge {
            schemas.extend(knowledge.readonly().tools().schemas());
        }
        if let Some(memory) = &self.memory {
            schemas.extend(memory.readonly().tools().schemas());
        }
        schemas
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        if name == "delegate" && self.spawn.is_some() {
            return Some(CodingToolAction::ExternalTool {
                plugin_id: "agent".to_owned(),
                tool_name: name.to_owned(),
                arguments: arguments.clone(),
                parent_invocation_id: None,
                timeout_ms: 120_000,
            });
        }
        if matches!(name, "agent_message" | "progress_note") {
            return Some(CodingToolAction::ExternalTool {
                plugin_id: "agent".to_owned(),
                tool_name: name.to_owned(),
                arguments: arguments.clone(),
                parent_invocation_id: None,
                timeout_ms: 30_000,
            });
        }
        if name == "knowledge" {
            return self
                .knowledge
                .as_ref()
                .and_then(|knowledge| knowledge.readonly().tools().resolve(name, arguments));
        }
        if name == "memory" {
            return self
                .memory
                .as_ref()
                .and_then(|memory| memory.readonly().tools().resolve(name, arguments));
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
            if plugin_id == "knowledge" {
                return match &self.knowledge {
                    Some(knowledge) => {
                        knowledge
                            .readonly()
                            .dispatcher()
                            .validate_external(plugin_id, tool_name, arguments)
                            .await
                    }
                    None => Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        "knowledge is not available to this child",
                    )),
                };
            }
            if plugin_id == "memory" {
                return match &self.memory {
                    Some(memory) => {
                        memory
                            .readonly()
                            .dispatcher()
                            .validate_external(plugin_id, tool_name, arguments)
                            .await
                    }
                    None => Err(HarnessError::new(
                        ErrorCode::PolicyDenied,
                        "memory is not available to this child",
                    )),
                };
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
            if plugin_id == "knowledge" {
                return match &self.knowledge {
                    Some(knowledge) => {
                        knowledge
                            .readonly()
                            .dispatcher()
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
                        "knowledge is not available to this child",
                    )),
                };
            }
            if plugin_id == "memory" {
                return match &self.memory {
                    Some(memory) => {
                        memory
                            .readonly()
                            .dispatcher()
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
                        "memory is not available to this child",
                    )),
                };
            }
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
            if tool_name == "delegate" {
                let payload = self.delegate(arguments).await?;
                return Ok(ToolOutput::ExternalTool {
                    plugin_id: "agent".to_owned(),
                    tool_name: tool_name.to_owned(),
                    payload,
                    inflight: 1,
                });
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
    /// prime-agent's `createRlmSession`: a separate top-level agent with its
    /// own conversation, started with `prompt`. Only a session that runs in a
    /// background worker can start one.
    async fn create_session(&self, request: &Value) -> Result<Value, String> {
        const SUPPORTED: [&str; 4] = ["name", "model", "thinking", "cwd"];
        let prompt = request["prompt"].as_str().unwrap_or_default();
        let kwargs = request["kwargs"].as_object().cloned().unwrap_or_default();
        let mut unsupported = kwargs
            .keys()
            .filter(|key| !SUPPORTED.contains(&key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            unsupported.sort();
            return Err(format!(
                "Unsupported rlm.create_session kwargs: {}",
                unsupported.join(", ")
            ));
        }
        if prompt.trim().is_empty() {
            return Err("rlm.create_session prompt must not be empty".to_owned());
        }
        let host = self
            .agents
            .shared
            .session_host
            .lock()
            .ok()
            .and_then(|host| host.clone())
            .ok_or("rlm.create_session requires a daemon-backed depth-0 session")?;
        let text = |key: &str| -> Result<Option<String>, String> {
            match kwargs.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if !value.trim().is_empty() => {
                    Ok(Some(value.trim().to_owned()))
                }
                Some(_) => Err(format!(
                    "rlm.create_session {key} must be a non-empty string"
                )),
            }
        };
        let session = super::agents::CreateSession {
            prompt: prompt.to_owned(),
            name: text("name")?,
            model: text("model")?,
            thinking: text("thinking")?,
            cwd: text("cwd")?.map(|cwd| self.launch.workspace_root.join(cwd)),
        };
        tokio::task::spawn_blocking(move || host.create_session(session))
            .await
            .map_err(|_| "rlm.create_session did not complete".to_owned())?
    }

    fn session_dir(&self) -> String {
        self.launch.workspace_root.display().to_string()
    }

    /// Find children by id or name; an empty selection means every child.
    fn select(&self, selectors: &[String]) -> Result<Vec<TaskId>, String> {
        let names = self.agents.shared.names(None);
        if selectors.is_empty() {
            return Ok(names.into_iter().map(|(task_id, ..)| task_id).collect());
        }
        selectors
            .iter()
            .map(|selector| {
                self.agents
                    .shared
                    .find(None, selector)
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
            .filter(|prompt| !prompt.is_empty() && prompt.chars().count() <= MAX_BRIEF_CHARS)
            .ok_or_else(|| format!("rlm.spawn needs a task of 1..={MAX_BRIEF_CHARS} characters"))?;
        let kwargs = &request["kwargs"];
        let name = kwargs["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("rlm.spawn needs a name")?
            .to_owned();
        let launch = self
            .launch
            .for_request(kwargs["model"].as_str(), kwargs["thinking"].as_str())?;
        let (task_id, name) = self
            .agents
            .start(
                AgentRole::Explorer,
                prompt.to_owned(),
                Some(name),
                &launch,
                None,
            )
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

    /// prime-agent's `rlm.rename`: the calling session, or a direct child
    /// chosen by handle, row or id. A child's name stays unique among its
    /// siblings, as `rlm.spawn` keeps it.
    fn rename(&self, request: &Value) -> Result<Value, String> {
        let name = request["name"]
            .as_str()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or("rlm.rename needs a non-empty name")?
            .to_owned();
        if name.chars().count() > 60 {
            return Err("a name is at most 60 characters".to_owned());
        }
        let Some(selector) = request["session_id"].as_str() else {
            if let Ok(mut identity) = self.agents.shared.root_identity.lock() {
                identity.1 = Some(name.clone());
            }
            let _ = self
                .agents
                .shared
                .sender
                .send(SessionEvent::RenameRequested { name: name.clone() });
            return Ok(json!({ "name": name }));
        };
        let task_id = self
            .select(&[selector.to_owned()])?
            .pop()
            .ok_or("no such child")?;
        let taken = self.agents.shared.children.lock().is_ok_and(|children| {
            children
                .iter()
                .any(|child| !child.deleted && child.name == name && child.task_id != task_id)
        });
        if taken {
            return Err(format!("a child named {name:?} already exists"));
        }
        self.agents
            .shared
            .update(&task_id, |child| child.name.clone_from(&name))
            .ok_or("no such child")?;
        Ok(json!({ "name": name }))
    }

    fn delete(&self, request: &Value) -> Result<Value, String> {
        let selector = request["target"].as_str().unwrap_or_default().to_owned();
        let task_id = self
            .select(std::slice::from_ref(&selector))?
            .pop()
            .ok_or("no such child")?;
        let dir = self.session_dir();
        let row = self
            .agents
            .shared
            .update(&task_id, |child| {
                if !child.state.settled() {
                    child.cancel_reason = Some("Deleted by parent orchestrator".to_owned());
                    child.cancellation.cancel();
                }
                child.deleted = true;
                json!({ "subagent": child.row(&dir) })
            })
            .ok_or_else(|| "no such child".to_owned());
        self.agents
            .shared
            .cancel_descendants(&task_id, "Deleted by parent orchestrator");
        self.agents.shared.persist_ledger();
        row
    }

    /// prime-agent's `rlm.find_models`: the credential-backed catalog models a
    /// child can run on, ranked against the query. Without a catalog the
    /// parent's model is the only one.
    fn models(&self, request: &Value) -> Result<Value, String> {
        let Some(query) = request["query"].as_str() else {
            return Err("rlm.find_models query must be a string".to_owned());
        };
        let limit = match &request["limit"] {
            Value::Null => Some(DEFAULT_RLM_MODEL_SEARCH_LIMIT),
            value => value
                .as_u64()
                .filter(|limit| (1..=MAX_RLM_MODEL_SEARCH_LIMIT).contains(limit)),
        }
        .ok_or_else(|| {
            format!(
                "rlm.find_models limit must be an integer from 1 to {MAX_RLM_MODEL_SEARCH_LIMIT}"
            )
        })?;
        let mut models = self
            .launch
            .models
            .as_ref()
            .map(|models| models.searchable())
            .unwrap_or_default();
        if models.is_empty() {
            let reference = &self.launch.model.reference;
            let (provider, id) = reference.split_once('/').unwrap_or(("ha", reference));
            models.push(SearchableModel {
                provider: provider.to_owned(),
                id: id.to_owned(),
                name: id.to_owned(),
            });
        }
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        Ok(json!({ "models": find_rlm_model_matches(query, &models, limit) }))
    }

    /// The session host, when this session runs as a background agent.
    fn host(&self) -> Option<Arc<dyn super::agents::SessionHost>> {
        self.agents
            .shared
            .session_host
            .lock()
            .ok()
            .and_then(|host| host.clone())
    }

    fn current_observation(&self) -> Value {
        let (id, name) = self
            .agents
            .shared
            .root_identity
            .lock()
            .map(|identity| identity.clone())
            .unwrap_or_default();
        json!({
            "sessionId": id.unwrap_or_else(|| "root".to_owned()),
            "sessionName": self.host().and_then(|host| host.agent_name()).or(name),
            "status": "running",
            "isSessionActive": true,
            "activity": {"kind": "executing", "tool_name": super::repl::TOOL_NAME},
        })
    }

    fn observe(&self, kind: &str, request: &Value) -> Result<Value, String> {
        if kind == "agent_observe.list" {
            let mut agents = self.host().map(|host| host.siblings()).unwrap_or_default();
            let children = self
                .agents
                .shared
                .children
                .lock()
                .map_err(|_| "the child registry is unavailable")?;
            agents.extend(
                children
                    .iter()
                    .filter(|child| !child.deleted && child.parent.is_none())
                    .map(ChildRecord::observation),
            );
            return Ok(json!({"current": self.current_observation(), "agents": agents}));
        }
        let target = request["target"].as_str().unwrap_or_default().to_owned();
        let ids = self.select(std::slice::from_ref(&target))?;
        let task_id = ids.first().ok_or("no such child")?;
        let children = self
            .agents
            .shared
            .children
            .lock()
            .map_err(|_| "the child registry is unavailable")?;
        let child = children
            .iter()
            .find(|child| &child.task_id == task_id)
            .ok_or("no such child")?;
        if kind == "agent_observe.get" {
            return Ok(json!({"agent": child.observation()}));
        }
        let bound = |key: &str, default, min, max| {
            request
                .get(key)
                .map_or(Some(default), Value::as_u64)
                .filter(|value| (min..=max).contains(value))
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    format!("agent_observe.recent {key} must be an integer from {min} to {max}")
                })
        };
        let limit = bound("limit", 8, 1, 50)?;
        let max_chars = bound("max_chars", 800, 80, 2000)?;
        let messages = child
            .history
            .iter()
            .rev()
            .take(limit)
            .map(|message| message_preview(message, max_chars))
            .collect::<Vec<_>>();
        let messages = if messages.is_empty() {
            match &child.state {
                ChildState::Done { answer, .. } | ChildState::Partial { answer, .. } => vec![
                    json!({"role": "assistant", "text": answer.chars().take(max_chars).collect::<String>()}),
                ],
                _ => Vec::new(),
            }
        } else {
            messages.into_iter().rev().collect()
        };
        Ok(json!({"target": target, "messages": messages}))
    }
}

fn message_preview(message: &ProviderMessage, max_chars: usize) -> Value {
    json!({
        "role": match message.role {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            _ => "system",
        },
        "text": message.content.chars().take(max_chars).collect::<String>(),
    })
}

fn clip(text: &str, whole: Option<&Path>) -> String {
    if text.chars().count() <= RLM_ANSWER_MAX_CHARS {
        return text.to_owned();
    }
    let kept = text.chars().take(RLM_ANSWER_MAX_CHARS).collect::<String>();
    match whole {
        Some(path) => format!(
            "{kept}\n[answer cut at {RLM_ANSWER_MAX_CHARS} characters; the whole answer is in {}]",
            path.display()
        ),
        None => format!("{kept}\n[answer cut at {RLM_ANSWER_MAX_CHARS} characters]"),
    }
}

/// Keep a child's whole answer when a notice cannot carry it: the parent reads
/// the rest from the file instead of losing it (an audit's last half was).
fn save_answer(dir: &Path, task_id: &TaskId, answer: &str) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join(format!("{}.md", task_id.as_str()));
    std::fs::write(&path, answer).ok()?;
    Some(path)
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
                "rlm.rename" => self.rename(request),
                "rlm.find_models" => self.models(request),
                "rlm.progress.note" => Err(
                    "progress notes are sent by child agents; this is the root agent".to_owned(),
                ),
                "rlm.create_session" => self.create_session(request).await,
                // prime-agent's `agent_observe` skill over this session's children:
                // the only family the root agent has here.
                "agent_observe.list" | "agent_observe.get" | "agent_observe.recent" => {
                    self.observe(kind, request)
                }
                // The root agent has no parent; its siblings are the other
                // top-level agents, and it messages its children.
                "agent_message.send" => {
                    let role = request["receiver_role"].as_str().unwrap_or_default();
                    match (role, self.host()) {
                        ("sibling", Some(host)) => message_root_sibling(host.as_ref(), request),
                        ("sibling", None) => Err(
                            "this agent runs without the background service, so it has no siblings"
                                .to_owned(),
                        ),
                        _ => self.agents.shared.send_from_parent(request).await,
                    }
                }
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
            // The launch stays: a message can wake the child for another run.
            let Some(launch) = self
                .shared
                .launches
                .lock()
                .ok()
                .and_then(|launches| launches.get(&task_id).cloned())
            else {
                return WorkerOutcome::Failed {
                    error: OrchestratorError::new(
                        ErrorCode::RoleUnavailable,
                        "the child's launch settings are missing",
                    ),
                };
            };
            let (can_spawn, depth) = self
                .shared
                .update(&task_id, |child| {
                    (child.depth < child.max_depth, child.depth)
                })
                .unwrap_or((false, 1));
            // A verifier judges what it is shown; it does not hand the judging on.
            let can_spawn = can_spawn && request.brief.role != AgentRole::Verifier;
            let hashline = launch.hashline.applies(&launch.model.reference);
            let spawn_line = if can_spawn {
                " You can split your work with the delegate tool: it starts explorer children of your own, and you are not finished until they are - their results reach you as [child-exited ...] or [child-failed ...] messages."
            } else {
                ""
            };
            let (workspace_root, policy, schemas, system_policy) = match request.brief.role {
                AgentRole::Explorer => (
                    launch.workspace_root.clone(),
                    child_policy(&launch.parent_policy),
                    harness_tools::coding_tool_schemas_for(hashline),
                    format!(
                        "You are a child agent spawned by your parent agent. Task prompts are labeled `[task from parent]`.{} Use agent_message to tell your parent what you found when it helps before you finish. Once your task is done, a message that wakes you again sends your parent no notice when you stop: answer it with agent_message when it asks for something, and send nothing when you would only repeat a status it already has.",
                        if can_spawn {
                            spawn_line
                        } else {
                            " You cannot delegate again."
                        }
                    ),
                ),
                AgentRole::Verifier => (
                    launch.workspace_root.clone(),
                    verifier_policy(&child_policy(&launch.parent_policy)),
                    verifier_schemas(),
                    super::verify::VERIFIER_POLICY.to_owned(),
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
                        child_policy(&launch.parent_policy),
                        harness_tools::coding_tool_schemas_for(hashline),
                        format!(
                            "You are a delegated coder. Work only in the assigned isolated M8-03 worktree and follow the brief. Tool calls use the host approval and policy service. Leave your changes in that worktree (committed or not) and report what changed, with paths; do not merge, push or copy them into the user's checkout and do not claim they are there - your parent brings them in with integrate_children, which merges every coder's branch, runs the project checks on the result and applies it only if the user's checkout has not changed.{spawn_line}"
                        ),
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
            // `[agents] max_workers`: past it a root's child waits here, its
            // row saying queued, until a running one ends. Unset, none waits.
            let _slot = if request.depth <= 1 {
                tokio::select! {
                    slot = self.shared.worker_slot() => Some(slot),
                    () = request.cancellation.cancelled() => None,
                }
            } else {
                None
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
            let mut session_id = SessionId::generate();
            let backlog = self
                .shared
                .update(&task_id, |child| {
                    child.session_id = Some(session_id.clone());
                    "starting".clone_into(&mut child.activity);
                    std::mem::take(&mut child.backlog)
                })
                .unwrap_or_default();
            // A child's own children work where it works, under its policy and
            // with its tools.
            let spawn = can_spawn.then(|| ChildSpawn {
                launch: ChildLaunch {
                    workspace_root: workspace_root.clone(),
                    workspace: request.brief.workspace.clone(),
                    parent_policy: policy.clone(),
                    knowledge: launch
                        .knowledge
                        .as_ref()
                        .map(super::knowledge::KnowledgeHost::readonly),
                    ..launch.clone()
                },
                cancellation: request.cancellation.clone(),
            });
            let child_tools = Arc::new(ChildTools {
                shared: Arc::clone(&self.shared),
                task_id: task_id.clone(),
                web: launch.web.clone(),
                knowledge: launch
                    .knowledge
                    .as_ref()
                    .map(super::knowledge::KnowledgeHost::readonly),
                memory: launch
                    .memory
                    .as_ref()
                    .map(super::memory::MemoryHost::readonly),
                spawn,
            });
            let tools = ToolExecutionService::new(Arc::clone(&store))
                .with_hashline(hashline)
                .with_policy(policy)
                .with_hooks(launch.hooks.clone())
                .with_external(Arc::clone(&child_tools) as Arc<dyn ExternalToolDispatcher>);
            let guard = self
                .shared
                .guard
                .lock()
                .ok()
                .and_then(|guard| guard.clone());
            let tools = match guard {
                Some(guard) => tools.with_guard(guard),
                None => tools,
            };
            let mut schemas = schemas;
            schemas.extend(child_tools.schemas());
            // The model the child calls, which `/subagent-model` can move from
            // its next call. Registered before the launch is read again, so a
            // change made in between reaches this run one way or the other.
            let live = Arc::new(LiveModel::new(launch.model.clone()));
            if let Ok(mut models) = self.shared.live.lock() {
                models.insert(task_id.clone(), Arc::clone(&live));
            }
            if let Some(current) = self
                .shared
                .launches
                .lock()
                .ok()
                .and_then(|launches| launches.get(&task_id).map(|launch| launch.model.clone()))
            {
                live.set(current);
            }
            let runtime = Arc::new(RuntimeService::new(
                Arc::clone(&store),
                Arc::clone(&live) as Arc<dyn ModelProvider>,
                launch.runtime_config.clone(),
            ));
            // The child's task is the parent's work, not a conversation of the
            // user's: `/resume` and "latest" leave it out.
            let _ = store
                .set_session_setting(&task_id, DELEGATED_CHILD_SETTING, &role_name)
                .await;
            // A woken child's run opens with the message that woke it, over
            // its conversation so far; a new one with its task, which
            // prime-agent frames as `[task from parent]`.
            let (wake, history) = self
                .shared
                .update(&task_id, |child| (child.wake.take(), child.history.clone()))
                .unwrap_or_default();
            let mut prompt = wake.unwrap_or_else(|| {
                format!(
                    "{CHILD_PROMPT_HEAD}\n\nAnswer the delegated task below. Return concise results and identify changed files or findings with paths.\n\nBrief:\n{}",
                    request.brief.objective
                )
            });
            for message in backlog {
                prompt.push_str("\n\n");
                prompt.push_str(&message);
            }
            let driver = TurnDriver::new(Arc::clone(&runtime), tools)
                .as_subagent()
                .with_external(ExternalTools::new(
                    Arc::clone(&child_tools) as Arc<dyn ExternalToolCatalog>
                ))
                .with_inbox(RunInbox::new(Arc::clone(&store)));
            // An explorer reads more than anyone: its long results are judged
            // as the root's are.
            let judges = self
                .shared
                .results
                .lock()
                .ok()
                .and_then(|judges| judges.clone());
            let driver = match judges {
                Some(judges) => driver
                    .with_visibility(judges.visibility)
                    .with_admission(judges.admission)
                    .with_completion_check(judges.completion),
                None => driver,
            };
            // A child's question is shown to the user, named after the child, and
            // the child waits for the answer.
            let question_host = self
                .shared
                .question_host
                .lock()
                .ok()
                .and_then(|host| host.clone());
            let driver = match question_host {
                Some(inner) => driver.with_question_host(Arc::new(ChildQuestionHost {
                    // The child's own name: several children share a role.
                    role: self
                        .shared
                        .update(&task_id, |child| child.name.clone())
                        .unwrap_or_else(|| role_name.clone()),
                    inner,
                })),
                None => driver,
            };
            let observer = Arc::new(ExplorerObserver {
                task_id: task_id.clone(),
                child_name: self
                    .shared
                    .update(&task_id, |child| child.name.clone())
                    .unwrap_or_else(|| role_name.clone()),
                shared: Arc::clone(&self.shared),
                ledger: self
                    .shared
                    .ledger
                    .lock()
                    .ok()
                    .and_then(|ledger| ledger.clone()),
                live: Some(Arc::clone(&live)),
                cancellation: request.cancellation.clone(),
                budget_error: Mutex::new(None),
                sender: self.shared.sender.clone(),
                token_usage: Mutex::new((0_u64, 0_u64)),
                cost_tracker: Mutex::new(CostTracker::default()),
                text: Mutex::new(String::new()),
                calls: Mutex::new(HashMap::new()),
            });
            let approval: Arc<dyn ApprovalGate> = Arc::new(ChildApprovalGate {
                role: role_name.clone(),
                inner: Arc::clone(&self.shared.approval_gate),
            });
            // The child's turns: its task, then - while children of its own
            // report back - one more turn over what they said, until its whole
            // subtree is quiet (prime-agent's `waitForRlmQuiescence`).
            let mut conversation = history;
            let mut steps = 0_u32;
            let mut tool_calls = 0_u32;
            let mut executions = Vec::new();
            let mut last_verified = None;
            let mut follow_up_error = None;
            let outcome = loop {
                self.shared.update(&task_id, |child| child.in_turn = true);
                let mut project_rules = launch.context.clone();
                if let Some(source) = &launch.digest {
                    project_rules.insert(0, source.child_block(&request.brief.objective));
                }
                if let Some(knowledge) = &launch.knowledge
                    && let Some(block) = super::knowledge::context_block(
                        knowledge,
                        &[(&request.brief.objective, 1.0)],
                    )
                {
                    project_rules.push(block);
                }
                let run_request = RunRequest::new(
                    session_id.clone(),
                    task_id.clone(),
                    // The run admits the prompt once, as the child's input and as
                    // proposed by the parent's model. Admitting the brief here as
                    // well put two contents under one input id, and every child
                    // failed with `idempotency_conflict` before its first step.
                    InputId::generate(),
                    prompt.clone(),
                    request.brief.workspace.clone(),
                )
                .with_system_policy(system_policy.clone())
                .with_project_rules(project_rules)
                .with_tool_schemas(schemas.clone())
                .with_authority(SourceAuthority::ModelProposed)
                .with_conversation(conversation.clone());
                let result = driver
                    .run_turn(
                        run_request,
                        TurnOptions {
                            workspace_root: workspace_root.clone(),
                            actor_id: format!("interactive.child.{role_name}"),
                            approvals: ApprovalMode::Ask(Arc::clone(&approval)),
                            limits: launch.child_limits,
                        },
                        observer.clone(),
                        request.cancellation.clone(),
                    )
                    .await;
                self.shared.update(&task_id, |child| child.in_turn = false);
                // Sends already admitted to this inbox can finish after its
                // final poll. New sends now go to pending, so wait for those
                // admitted sends, then carry only commands the driver did not
                // claim into the next child turn.
                loop {
                    let changed = self.shared.changed.notified();
                    if self
                        .shared
                        .update(&task_id, |child| child.in_flight_messages)
                        .unwrap_or_default()
                        == 0
                    {
                        break;
                    }
                    changed.await;
                }
                let carried = async {
                    let inbox = RunInbox::new(Arc::clone(&store));
                    let Some(run) = store.latest_run(&session_id).await? else {
                        return Ok::<(), harness_runtime::RuntimeError>(());
                    };
                    loop {
                        let now = harness_runtime::now_unix_ms();
                        let commands = inbox.claim(&run, 64, now).await?;
                        let count = commands.len();
                        for command in commands {
                            if command.kind == harness_store_sqlite::RunCommandKind::Steer {
                                if let Some(text) = RunInbox::steering_text(&command) {
                                    inbox
                                        .apply(&command, "carried to the next child turn", now)
                                        .await?;
                                    self.shared
                                        .update(&task_id, |child| child.pending.push(text));
                                } else {
                                    inbox.reject(&command, "message has no text", now).await?;
                                }
                            } else {
                                inbox
                                    .apply(&command, "the child turn already stopped", now)
                                    .await?;
                            }
                        }
                        if count < 64 {
                            break;
                        }
                    }
                    Ok(())
                }
                .await;
                // A later wake opens a new session of this task. Release this
                // turn's lease before its child can settle and be woken again.
                // An admission conflict belongs to another session; leave it.
                let admitted = match &result {
                    Ok(_) => true,
                    Err(error) if error.code() == ErrorCode::TaskLeaseConflict => false,
                    Err(_) => matches!(store.session_summary(&session_id).await, Ok(Some(_))),
                };
                let released = if admitted {
                    store
                        .release_task_lease(&task_id)
                        .await
                        .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))
                } else {
                    Ok(())
                };
                let result = result
                    .map_err(|error| OrchestratorError::new(error.code(), error.to_string()))
                    .and_then(|outcome| {
                        carried.map(|()| outcome).map_err(|error| {
                            OrchestratorError::new(error.code(), error.to_string())
                        })
                    })
                    .and_then(|outcome| released.map(|()| outcome));
                let cost = observer
                    .cost_tracker
                    .lock()
                    .map_or_else(|_| "n/a".to_owned(), |tracker| tracker.display());
                self.shared.update(&task_id, |child| child.cost = cost);
                let result = if let Some(error) = observer
                    .budget_error
                    .lock()
                    .ok()
                    .and_then(|error| error.clone())
                {
                    Err(error)
                } else {
                    result
                };
                let outcome = match result {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        if let Some(last) = last_verified {
                            follow_up_error = Some(error);
                            break last;
                        }
                        return WorkerOutcome::Failed { error };
                    }
                };
                steps += outcome.steps;
                tool_calls += outcome.tool_calls;
                executions.extend(outcome.executions.iter().cloned());
                self.shared
                    .update(&task_id, |child| child.tool_calls = tool_calls);
                if outcome.stop == TurnStop::Canceled || request.cancellation.is_cancelled() {
                    return WorkerOutcome::OutcomeUnknown {
                        reason: "the child was stopped".to_owned(),
                    };
                }
                // A child that stopped short of an answer - a loop, an
                // unverifiable reply, a bound - failed, and says why. It used to be
                // reported as completed ("Explorer completed without a final text
                // answer"), so the parent could not tell a child that broke from one
                // that finished.
                if let Some(reason) = stopped_short(outcome.stop) {
                    let error = OrchestratorError::new(
                        ErrorCode::ResultIncomplete,
                        format!(
                            "{role_name} stopped without an answer: {reason} ({steps} step(s), {tool_calls} tool call(s))"
                        ),
                    );
                    if let Some(last) = last_verified {
                        follow_up_error = Some(error);
                        break last;
                    }
                    return WorkerOutcome::Failed { error };
                }
                let Some(next) = self.shared.quiet(&task_id, &request.cancellation).await else {
                    break outcome;
                };
                conversation.push(ProviderMessage::new(MessageRole::User, prompt));
                conversation.push(ProviderMessage::new(
                    MessageRole::Assistant,
                    outcome.final_text.clone(),
                ));
                // Only a terminal, nonempty answer can survive a failed follow-up.
                // Never replace it with text from an unverified turn.
                if !outcome.final_text.trim().is_empty() {
                    last_verified = Some(outcome);
                }
                prompt = next.join("\n\n");
                session_id = SessionId::generate();
                self.shared.update(&task_id, |child| {
                    child.session_id = Some(session_id.clone());
                });
            };
            if request.cancellation.is_cancelled() {
                return WorkerOutcome::OutcomeUnknown {
                    reason: "the child was stopped".to_owned(),
                };
            }
            // The conversation so far, for the turn a later message wakes it for.
            let mut history = conversation.clone();
            history.push(ProviderMessage::new(MessageRole::User, prompt.clone()));
            history.push(ProviderMessage::new(
                MessageRole::Assistant,
                outcome.final_text.clone(),
            ));
            self.shared
                .update(&task_id, |child| child.history = history);
            // A failed driver call has no TurnOutcome, but the observer already
            // counted any tools it started. Do not erase those counts at settle.
            let tool_calls = self
                .shared
                .update(&task_id, |child| child.tool_calls.max(tool_calls))
                .unwrap_or(tool_calls);
            let receipts = serde_json::to_vec(&executions).unwrap_or_default();
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
                outcome: if follow_up_error.is_some() {
                    DelegatedOutcome::Failed
                } else {
                    DelegatedOutcome::Completed
                },
                summary,
                artifact_refs: Vec::new(),
                base_revision: request.brief.base_commit.clone(),
                result_revision: request.brief.base_commit.clone(),
                checked_revisions: Vec::new(),
                check_receipts: Vec::new(),
                usage: BudgetUsage {
                    model_requests: steps,
                    retries: 0,
                },
                detail: json!({
                    "partial": follow_up_error.is_some(),
                    "error": follow_up_error.as_ref().map(ToString::to_string),
                    "failed_session_id": follow_up_error.as_ref().map(|_| &session_id),
                    "session_id": outcome.session_id,
                    "depth": depth,
                    "steps": steps,
                    "tool_calls": tool_calls,
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
    /// The child's own name (`coder-c0cc21d5`), which its notices and pane
    /// lines carry: several children share a role.
    child_name: String,
    shared: Arc<AgentsShared>,
    ledger: Option<Arc<BudgetLedger>>,
    /// The model the child runs on now, for the price of each call.
    live: Option<Arc<LiveModel>>,
    cancellation: CancellationToken,
    budget_error: Mutex<Option<OrchestratorError>>,
    sender: UnboundedSender<SessionEvent>,
    token_usage: Mutex<(u64, u64)>,
    cost_tracker: Mutex<CostTracker>,
    /// The answer's text since its last whole line, for the child's pane.
    text: Mutex<String>,
    /// When each running call started, by its id: its pane shows how long it took.
    calls: Mutex<HashMap<String, Instant>>,
}

/// How many lines of a tool's output a child's pane is sent.
const CHILD_OUTPUT_LINES: usize = 8;

impl ExplorerObserver {
    /// Send the child's pane some lines.
    fn pane(&self, kind: super::events::ChildLineKind, lines: Vec<String>, call: &str) {
        if lines.is_empty() {
            return;
        }
        let _ = self.sender.send(SessionEvent::ChildOutput {
            name: self.child_name.clone(),
            kind,
            lines,
            call: call.to_owned(),
        });
    }

    /// Send the answer's whole lines; with `all`, the unfinished one too.
    fn flush_text(&self, all: bool) {
        let lines = {
            let Ok(mut text) = self.text.lock() else {
                return;
            };
            let cut = if all {
                text.len()
            } else {
                text.rfind('\n').map_or(0, |at| at + 1)
            };
            let whole: String = text.drain(..cut).collect();
            whole
                .lines()
                .map(str::trim_end)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        self.pane(super::events::ChildLineKind::Text, lines, "");
    }
}

impl TurnObserver for ExplorerObserver {
    #[allow(clippy::too_many_lines, reason = "one arm per kind of progress")]
    fn observe(&self, progress: TurnProgress) {
        match progress {
            TurnProgress::StepStarted { step } => {
                self.flush_text(true);
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
                    child.activity_tool = None;
                });
            }
            TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => {
                if let Ok(mut usage) = self.token_usage.lock() {
                    usage.0 = usage.0.saturating_add(prompt_tokens);
                    usage.1 = usage.1.saturating_add(completion_tokens);
                }
                let usage = CostUsage {
                    input_tokens: prompt_tokens,
                    output_tokens: completion_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                };
                let price = self.live.as_ref().and_then(|live| live.get().price);
                self.shared.charge(price, usage);
                if let Ok(mut tracker) = self.cost_tracker.lock() {
                    tracker.record(price, usage);
                    let cost = tracker.display();
                    self.shared.update(&self.task_id, |child| child.cost = cost);
                }
            }
            TurnProgress::ToolStarted {
                name,
                summary,
                call_id,
                ..
            } => {
                self.flush_text(true);
                self.shared.update(&self.task_id, |child| {
                    child.tool_calls += 1;
                    child.activity = format!("{name}: {summary}");
                    child.activity_tool = Some(name.clone());
                });
                if let Ok(mut calls) = self.calls.lock() {
                    calls.insert(call_id.clone(), Instant::now());
                }
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {name}: {summary}", self.child_name),
                });
                self.pane(
                    super::events::ChildLineKind::Call,
                    vec![format!("{name}: {summary}")],
                    &call_id,
                );
            }
            TurnProgress::ToolSettled {
                name,
                ok,
                detail,
                call_id,
            } => {
                let millis = self
                    .calls
                    .lock()
                    .ok()
                    .and_then(|mut calls| calls.remove(&call_id))
                    .map_or(0, |started| started.elapsed().as_millis());
                let settled = if ok {
                    format!("ok {millis}")
                } else {
                    let detail = detail.unwrap_or_else(|| "no detail".to_owned());
                    let _ = self.sender.send(SessionEvent::Notice {
                        message: format!("[child {}] {name} failed: {detail}", self.child_name),
                    });
                    format!(
                        "failed {millis}: {}",
                        detail.lines().next().unwrap_or_default()
                    )
                };
                self.pane(
                    super::events::ChildLineKind::Settled,
                    vec![settled],
                    &call_id,
                );
            }
            TurnProgress::Notice(message) => {
                let _ = self.sender.send(SessionEvent::Notice {
                    message: format!("[child {}] {message}", self.child_name),
                });
            }
            TurnProgress::TextDelta(delta) => {
                let whole_line = delta.contains('\n');
                if let Ok(mut text) = self.text.lock() {
                    text.push_str(&delta);
                }
                if whole_line {
                    self.flush_text(false);
                }
            }
            TurnProgress::ToolOutput { text, call_id, .. } => {
                let lines = text.lines().filter(|line| !line.trim().is_empty());
                let total = lines.clone().count();
                let mut shown = lines
                    .take(CHILD_OUTPUT_LINES)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if total > CHILD_OUTPUT_LINES {
                    shown.push(format!("… {} more lines", total - CHILD_OUTPUT_LINES));
                }
                self.pane(super::events::ChildLineKind::Output, shown, &call_id);
            }
            TurnProgress::ThinkingDelta(_)
            | TurnProgress::StreamRestarted
            | TurnProgress::ToolCallStreaming { .. }
            | TurnProgress::ToolProgress { .. }
            | TurnProgress::Info(_) => {}
        }
    }
}

struct ChildApprovalGate {
    role: String,
    inner: Arc<dyn ApprovalGate>,
}

/// A child's question, named after the child, as its approvals are.
struct ChildQuestionHost {
    role: String,
    inner: Arc<dyn harness_tools::QuestionHost>,
}

impl harness_tools::QuestionHost for ChildQuestionHost {
    fn ask(
        &self,
        question: String,
        options: Vec<String>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, HarnessError>> + Send>>
    {
        self.inner
            .ask(format!("[child {}] {question}", self.role), options)
    }
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
    use harness_tools::{
        CodingToolAction, ToolPatternRule, TurnObserver, TurnProgress, coding_tool_schemas,
    };
    use harness_types::{AgentRunId, ErrorCode, ProjectId, TaskId};

    use super::{
        AgentsShared, ChildLaunch, ChildModel, ChildModels, ChildState, ChildTools,
        DelegateCatalog, ExplorerObserver, SessionAgents, child_policy, inspect_coder_input,
        message_arguments, outcome_state, state_payload, stopped_short, terminal_notice,
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

    /// A child has its parent's permissions, as a prime-agent child does: in a
    /// `full-auto` session it reads, writes and runs commands without a panel,
    /// and what the parent may not do it may not do either.
    #[test]
    fn a_child_has_its_parents_permissions() {
        let parent = harness_tools::ToolPolicy::new(1, Vec::new())
            .with_mode(harness_tools::PolicyMode::FullAuto);
        let policy = child_policy(&parent);
        let actions = [
            CodingToolAction::ReadFile {
                path: "src/lib.rs".to_owned(),
                offset: None,
                limit: None,
                tail: None,
            },
            CodingToolAction::WriteFile {
                path: "note.txt".to_owned(),
                content: "x".to_owned(),
                expected_hash: None,
            },
            CodingToolAction::RunShell {
                command: "echo hi".to_owned(),
                timeout_ms: 1000,
                isolation: harness_tools::IsolationMode::BestEffort,
                env: Vec::new(),
            },
        ];
        for action in &actions {
            assert_eq!(
                format!("{:?}", policy.decide(action)),
                format!("{:?}", parent.decide(action)),
                "{action:?}"
            );
            assert!(policy.denial_for(action).is_none(), "{action:?}");
        }
        let denied = harness_tools::ToolPolicy::new(1, Vec::new())
            .with_tool_rules(vec![ToolPatternRule::deny("run_shell*", "not here")]);
        assert!(child_policy(&denied).denial_for(&actions[2]).is_some());
    }

    #[test]
    fn a_child_has_its_parents_tools() {
        let names = coding_tool_schemas()
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        for tool in ["read_file", "write_file", "run_shell", "apply_patch"] {
            assert!(names.iter().any(|name| name == tool), "{tool}");
        }
        let delegate = ChildTools::delegate_schema();
        assert_eq!(
            delegate["function"]["parameters"]["properties"]["role"]["enum"],
            serde_json::json!(["explorer"])
        );
    }

    #[tokio::test]
    async fn a_child_recalls_the_roots_memory_but_cannot_change_it() {
        use harness_tools::{ExternalToolCatalog, ExternalToolDispatcher};
        use serde_json::json;

        let root = tempfile::tempdir().expect("root");
        let data = root.path().join("data");
        let conversation = TaskId::generate();
        // The root keeps a preference in its own conversation.
        let memory = crate::interactive::memory::MemoryHost::new(&data, conversation.as_str());
        let local = crate::interactive::harness::local_dir(&data, conversation.as_str());
        std::fs::create_dir_all(&local).expect("local");
        std::fs::write(
            local.join("harness_state.json"),
            json!({"schema":1,"entries":{"memory":{"tabs":{"id":"tabs","kind":"memory","title":"Indentation","content":"This project indents with tabs","version":1}}}}).to_string(),
        )
        .expect("state");
        let (sender, _events) = tokio::sync::mpsc::unbounded_channel();
        let tools = ChildTools {
            shared: shared(sender),
            // The child's own task: memory still reads the root's conversation.
            task_id: TaskId::generate(),
            web: None,
            knowledge: None,
            // The writable root host: the boundary makes it read-only.
            memory: Some(memory),
            spawn: None,
        };
        let schema = tools
            .schemas()
            .into_iter()
            .find(|schema| schema["function"]["name"] == "memory")
            .expect("memory schema");
        assert_eq!(
            schema["function"]["parameters"]["properties"]["action"]["enum"],
            json!(["list", "search", "read", "history"])
        );
        let search = json!({"action":"search","query":"tabs indentation"});
        let action = tools.resolve("memory", &search).expect("resolves");
        let CodingToolAction::ExternalTool { plugin_id, .. } = &action else {
            panic!("{action:?}");
        };
        assert_eq!(plugin_id, "memory");
        tools
            .validate_external("memory", "memory", &search)
            .await
            .expect("search validates");
        // What the child's search reads: the root's conversation.
        let found = tools
            .memory
            .as_ref()
            .expect("memory")
            .readonly()
            .search_for_test(&search);
        assert_eq!(found["hits"][0]["id"], "tabs", "{found}");
        for forged in [
            json!({"action":"write","scope":"global","title":"x","content":"y"}),
            json!({"action":"delete","id":"tabs","expected_version":1}),
        ] {
            let error = tools
                .validate_external("memory", "memory", &forged)
                .await
                .expect_err("a child cannot change memory");
            assert!(error.message().contains("read-only"), "{error}");
        }
    }

    #[tokio::test]
    async fn child_knowledge_reads_are_available_but_forged_mutations_and_off_are_refused() {
        use harness_tools::{ExternalToolCatalog, ExternalToolDispatcher};
        use serde_json::json;

        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let knowledge = crate::interactive::knowledge::KnowledgeHost::new(
            &workspace,
            &root.path().join("data"),
        );
        knowledge.command("on").expect("enable project knowledge");
        let (sender, _events) = tokio::sync::mpsc::unbounded_channel();
        let tools = ChildTools {
            shared: shared(sender),
            task_id: TaskId::generate(),
            web: None,
            // Deliberately pass the writable root host: the child boundary must
            // enforce read-only even if a caller forgets to make it so.
            knowledge: Some(knowledge.clone()),
            memory: None,
            spawn: None,
        };
        let schemas = tools.schemas();
        let schema = schemas
            .iter()
            .find(|schema| schema["function"]["name"] == "knowledge")
            .expect("knowledge schema");
        assert_eq!(
            schema["function"]["parameters"]["properties"]["action"]["enum"],
            json!(["list", "search", "read", "graph"])
        );
        for action in ["list", "search", "read", "graph"] {
            let args = json!({"action":action,"scope":"project","query":"architecture","path":"architecture.md"});
            assert!(tools.resolve("knowledge", &args).is_some(), "{args}");
            tools
                .validate_external("knowledge", "knowledge", &args)
                .await
                .expect("read validation");
        }
        for action in ["write", "delete"] {
            let args = json!({"action":action,"path":"architecture.md","title":"Architecture",
                "summary":"Facts","content":"Do not write","expected_hash":"fake"});
            assert!(tools.resolve("knowledge", &args).is_none());
            let error = tools
                .validate_external("knowledge", "knowledge", &args)
                .await
                .expect_err("forged mutation refused");
            assert!(error.message().contains("read-only"), "{error}");
        }
        let args = json!({"action":"list","scope":"project"});
        assert!(tools.resolve("knowledge", &args).is_some());
        knowledge.command("off").expect("disable project knowledge");
        assert!(tools.resolve("knowledge", &args).is_none());
        assert!(
            tools
                .validate_external("knowledge", "knowledge", &args)
                .await
                .is_err()
        );
        assert!(
            tools
                .validate_external("knowledge", "other", &args)
                .await
                .is_err()
        );
        assert!(
            !workspace
                .join(".harness/knowledge/architecture.md")
                .exists()
        );
    }

    /// Every message between agents is kept for `/agents messages` and shown
    /// as it is sent.
    #[test]
    fn a_message_between_agents_is_kept_and_shown() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let shared = shared(sender);
        shared.record("reviewer", "coder", "check line 4");
        match events.try_recv() {
            Ok(crate::interactive::events::SessionEvent::AgentExchange { from, to, text }) => {
                assert_eq!(
                    (from.as_str(), to.as_str(), text.as_str()),
                    ("reviewer", "coder", "check line 4")
                );
            }
            other => panic!("an exchange event: {other:?}"),
        }
        let kept = shared.exchanges.lock().expect("exchanges");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "check line 4");
    }

    /// prime-agent's `find_models_round_trip_through_the_registry`: the exact
    /// selector first, then a prefix; an empty query lists every model.
    #[test]
    fn find_models_ranks_matches_as_prime_does() {
        let model = |provider: &str, id: &str, name: &str| super::SearchableModel {
            provider: provider.to_owned(),
            id: id.to_owned(),
            name: name.to_owned(),
        };
        let models = [
            model("test-provider", "glm-5.3-turbo", "GLM 5.3 Turbo"),
            model("test-provider", "glm-5.3", "GLM 5.3"),
            model("openai-codex", "gpt-5.3-codex-spark", "GPT-5.3 Codex Spark"),
        ];
        let found = super::find_rlm_model_matches("test-provider/glm-5.3", &models, 8);
        assert_eq!(
            found[0],
            serde_json::json!({
                "provider": "test-provider",
                "id": "glm-5.3",
                "name": "GLM 5.3",
                "selector": "test-provider/glm-5.3",
            })
        );
        assert_eq!(found[1]["selector"], "test-provider/glm-5.3-turbo");
        assert_eq!(found.len(), 2);
        let spark = super::find_rlm_model_matches("codex-spark", &models, 8);
        assert_eq!(spark[0]["selector"], "openai-codex/gpt-5.3-codex-spark");
        assert_eq!(super::find_rlm_model_matches("", &models, 8).len(), 3);
        assert_eq!(super::find_rlm_model_matches("", &models, 2).len(), 2);
        assert!(super::find_rlm_model_matches("claude", &models, 8).is_empty());
    }

    /// prime-agent's `rlm.spawn(thinking=...)`: a known level reaches the
    /// child's model; an unknown one fails the spawn.
    #[test]
    fn a_spawn_can_ask_for_a_thinking_level() {
        struct Levels;
        impl ChildModels for Levels {
            fn default_model(&self) -> Option<String> {
                None
            }
            fn resolve(&self, reference: &str) -> Result<ChildModel, String> {
                self.resolve_at(reference, None, false)
            }
            fn resolve_at(
                &self,
                reference: &str,
                level: Option<harness_providers::ThinkingLevel>,
                strict: bool,
            ) -> Result<ChildModel, String> {
                Ok(ChildModel {
                    provider: Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
                    reference: format!(
                        "{reference}@{}{}",
                        level.map_or("parent", harness_providers::ThinkingLevel::as_str),
                        if strict { "!" } else { "" }
                    ),
                    price: None,
                    thinking: None,
                })
            }
        }
        let repo = tempfile::tempdir().expect("repo");
        let mut launch = launch_for(
            repo.path(),
            Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
        );
        launch.models = Some(Arc::new(Levels));
        let parent = launch.model.reference.clone();
        assert_eq!(
            launch
                .for_request(None, None)
                .expect("parent")
                .model
                .reference,
            parent,
            "nothing asked: the parent's model and level"
        );
        assert_eq!(
            launch
                .for_request(None, Some("high"))
                .expect("high")
                .model
                .reference,
            format!("{parent}@high!")
        );
        let refused = launch
            .for_request(None, Some("huge"))
            .err()
            .expect("an unknown level fails the spawn");
        assert!(refused.contains("thinking must be one of"), "{refused}");
    }

    /// The parts of a session's children that need no running worker.
    fn shared(
        sender: tokio::sync::mpsc::UnboundedSender<crate::interactive::events::SessionEvent>,
    ) -> Arc<AgentsShared> {
        let gate = Arc::new(crate::interactive::service::ChannelApprovalGate::new(
            sender.clone(),
            Duration::from_secs(5),
        ));
        Arc::new(AgentsShared::new(
            SharedStore::new(std::env::temp_dir().join("ha-unused-store")),
            sender,
            gate,
        ))
    }

    /// A delegate's pane gets what the transcript does not: the first lines
    /// of each tool's output and its answer's whole lines as they are written.
    #[test]
    fn a_childs_output_and_answer_reach_its_pane() {
        use super::CHILD_OUTPUT_LINES;
        use crate::interactive::events::{ChildLineKind, SessionEvent};
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let observer = ExplorerObserver {
            task_id: TaskId::generate(),
            child_name: "explorer".to_owned(),
            shared: shared(sender.clone()),
            ledger: None,
            live: None,
            cancellation: CancellationToken::new(),
            budget_error: std::sync::Mutex::new(None),
            sender,
            token_usage: std::sync::Mutex::new((0, 0)),
            cost_tracker: std::sync::Mutex::new(CostTracker::default()),
            text: std::sync::Mutex::new(String::new()),
            calls: std::sync::Mutex::new(std::collections::HashMap::new()),
        };
        let output = (1..=10)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        observer.observe(TurnProgress::ToolOutput {
            name: "read_file".to_owned(),
            call_id: "c1".to_owned(),
            text: output,
        });
        observer.observe(TurnProgress::TextDelta("The flow ".to_owned()));
        observer.observe(TurnProgress::TextDelta("is safe.\nIt sa".to_owned()));
        observer.observe(TurnProgress::TextDelta("ves orders.".to_owned()));
        observer.observe(TurnProgress::StepStarted { step: 1 });
        let mut panes = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            if let SessionEvent::ChildOutput {
                name, kind, lines, ..
            } = event
            {
                assert_eq!(name, "explorer");
                panes.push((kind, lines));
            }
        }
        assert_eq!(panes.len(), 3, "{panes:?}");
        assert_eq!(panes[0].0, ChildLineKind::Output);
        assert_eq!(panes[0].1.len(), CHILD_OUTPUT_LINES + 1);
        assert_eq!(
            panes[0].1.last().map(String::as_str),
            Some("… 2 more lines")
        );
        assert_eq!(
            panes[1],
            (ChildLineKind::Text, vec!["The flow is safe.".to_owned()]),
            "a whole line goes as soon as it ends"
        );
        assert_eq!(
            panes[2],
            (ChildLineKind::Text, vec!["It saves orders.".to_owned()]),
            "the rest goes when the next step starts"
        );
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
            child_name: "explorer".to_owned(),
            shared: shared(sender.clone()),
            ledger: Some(Arc::clone(&ledger)),
            live: None,
            cancellation: CancellationToken::new(),
            budget_error: std::sync::Mutex::new(None),
            sender,
            token_usage: std::sync::Mutex::new((0, 0)),
            cost_tracker: std::sync::Mutex::new(CostTracker::default()),
            text: std::sync::Mutex::new(String::new()),
            calls: std::sync::Mutex::new(std::collections::HashMap::new()),
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
            terminal_notice("scout", &failed, false, None).as_deref(),
            Some("[child-failed child:scout]\n\nprovider_protocol: stream failed")
        );
        let stopped = ChildState::Cancelled {
            reason: "Deleted by parent orchestrator".to_owned(),
        };
        assert_eq!(
            terminal_notice("scout", &stopped, false, None).as_deref(),
            Some("[child-exited: cancelled child:scout]\n\nDeleted by parent orchestrator")
        );
        let bare = ChildState::Cancelled {
            reason: String::new(),
        };
        assert_eq!(
            terminal_notice("scout", &bare, false, None).as_deref(),
            Some("[child-exited: cancelled child:scout]")
        );
        let done = ChildState::Done {
            answer: "found it".to_owned(),
            detail: serde_json::Value::Null,
        };
        assert_eq!(
            terminal_notice("scout", &done, false, None).as_deref(),
            Some("[child-exited: no-reply child:scout]\n\nLast assistant text: found it")
        );
        assert_eq!(
            terminal_notice("scout", &done, true, None),
            None,
            "a child that replied to its parent needs no notice"
        );
        let silent = ChildState::Done {
            answer: String::new(),
            detail: serde_json::Value::Null,
        };
        assert_eq!(
            terminal_notice("scout", &silent, false, None).as_deref(),
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
                        thinking: None,
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
        // A verifier or a reviewer judges the parent's work: with no model named
        // it runs on the parent's model, not the children's default.
        for role in [super::AgentRole::Verifier, super::AgentRole::Reviewer] {
            assert_eq!(
                launch.for_role(role, None).expect("judge").model.reference,
                "deepseek/fixture-model",
                "{role:?}"
            );
        }
        assert_eq!(
            launch
                .for_role(super::AgentRole::Verifier, Some("known/asked"))
                .expect("asked")
                .model
                .reference,
            "known/asked",
            "a model named for the verifier wins"
        );
        for role in [super::AgentRole::Explorer, super::AgentRole::Coder] {
            assert_eq!(
                launch.for_role(role, None).expect("worker").model.reference,
                "known/default",
                "{role:?}"
            );
        }
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
                thinking: None,
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
            knowledge: None,
            memory: None,
            hashline: super::super::config::HashlineRule::Off,
            checks: Vec::new(),
            context: Vec::new(),
            digest: None,
            follows: None,
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

    #[test]
    fn a_dirty_refusal_names_a_few_changes_and_the_way_out() {
        let mut reasons = (0..33)
            .map(|index| super::DirtyReason::TrackedModification {
                path: format!("src/file{index}.ts"),
            })
            .collect::<Vec<_>>();
        reasons.push(super::DirtyReason::DetachedHead);
        let message = super::dirty_reasons(&reasons);
        assert!(
            message.starts_with("repository is in detached HEAD state"),
            "{message}"
        );
        assert!(message.contains("33 uncommitted change(s)"), "{message}");
        assert!(message.contains("src/file4.ts"), "{message}");
        assert!(!message.contains("src/file5.ts"), "{message}");
        assert!(message.contains("and 28 more"), "{message}");
        assert!(message.contains("commit them first"), "{message}");
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
        assert!(
            error.message().contains("1 uncommitted change(s)"),
            "{error}"
        );
        assert!(error.message().contains("commit them first"), "{error}");
        assert_eq!(
            std::fs::read_to_string(repo.join("uncommitted.txt")).expect("user change remains"),
            "preserve me\n"
        );
    }

    /// A coder that writes each `path=content` line of its brief into its
    /// worktree and reports what it did, as a real coder leaves its work.
    struct EditingCoder;

    impl harness_orchestrator::WorkerBackend for EditingCoder {
        fn dispatch(
            &self,
            request: harness_orchestrator::WorkerRequest,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = harness_orchestrator::WorkerOutcome> + Send + '_>,
        > {
            Box::pin(async move {
                let worktree = request.worktree.clone().expect("a coder has its worktree");
                for line in request.brief.objective.lines() {
                    if let Some((path, content)) = line.split_once('=') {
                        std::fs::write(
                            std::path::Path::new(&worktree.path).join(path),
                            format!("{content}\n"),
                        )
                        .expect("the coder writes in its worktree");
                    }
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
                        summary: format!("edited: {}", request.brief.objective),
                        artifact_refs: Vec::new(),
                        base_revision: request.brief.base_commit.clone(),
                        result_revision: request.brief.base_commit.clone(),
                        checked_revisions: Vec::new(),
                        check_receipts: Vec::new(),
                        usage: harness_orchestrator::BudgetUsage {
                            model_requests: 1,
                            retries: 0,
                        },
                        detail: serde_json::json!({
                            "worktree_id": worktree.worktree_id,
                            "worktree_path": worktree.path,
                            "branch": worktree.branch,
                        }),
                    },
                ))
            })
        }
    }

    /// A user's repository with `notes.txt`, the session's children over it,
    /// and the root agent's delegate tool.
    struct IntegrationBench {
        _temporary: tempfile::TempDir,
        repo: std::path::PathBuf,
        tool: super::DelegateTool,
    }

    impl IntegrationBench {
        fn new() -> Self {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let repo = temporary.path().join("repo");
            std::fs::create_dir_all(&repo).expect("repo directory");
            clean_repository(&repo);
            std::fs::write(repo.join("notes.txt"), "base\n").expect("notes");
            git(&repo, &["add", "notes.txt"]);
            git(&repo, &["commit", "-m", "notes"]);
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
                &(Arc::new(EditingCoder) as Arc<dyn harness_orchestrator::WorkerBackend>),
            );
            let launch = launch_for(
                &repo,
                Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
            );
            Self {
                _temporary: temporary,
                repo,
                tool: super::DelegateTool {
                    agents,
                    launch,
                    parent_cancellation: CancellationToken::new(),
                },
            }
        }

        /// Run a coder to the end; it edits per `brief`.
        async fn coder(&self, brief: &str) -> serde_json::Value {
            let result = self
                .tool
                .run(&serde_json::json!({"role": "coder", "brief": brief}))
                .await
                .expect("the coder runs");
            assert_eq!(result["status"], "completed", "{result}");
            result
        }

        async fn integrate(&self, arguments: serde_json::Value) -> serde_json::Value {
            self.tool
                .integrate(&arguments)
                .await
                .expect("the integration answers")
        }

        fn check(&mut self, command: &str) {
            self.tool.launch.checks = vec![crate::interactive::verify::Check {
                name: "merged".to_owned(),
                command: command.to_owned(),
                hint: None,
                timeout_ms: 60_000,
            }];
        }

        fn head(&self) -> String {
            let output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.repo)
                .output()
                .expect("git starts");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        fn status(&self) -> String {
            let output = Command::new("git")
                .args(["status", "--porcelain=v1", "--untracked-files=all"])
                .current_dir(&self.repo)
                .output()
                .expect("git starts");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        }

        /// A file of the user's checkout, whatever line endings Git's checkout
        /// settings gave it.
        fn read(&self, path: &str) -> Option<String> {
            std::fs::read_to_string(self.repo.join(path))
                .ok()
                .map(|text| text.replace("\r\n", "\n"))
        }
    }

    /// The parent read the paths its coders reported in its own checkout and
    /// was told only "File not found": the files were in the coders'
    /// worktrees, waiting for `integrate_children`.
    #[tokio::test]
    async fn a_file_a_coder_made_is_found_in_its_worktree_until_integrated() {
        use harness_tools::PathElsewhere as _;
        let bench = IntegrationBench::new();
        let coder = bench.coder("made.txt=from the coder").await;
        let name = coder["name"].as_str().unwrap_or_default().to_owned();
        let agents = &bench.tool.agents;
        let places = agents.elsewhere("made.txt");
        assert_eq!(places.len(), 1, "{places:?}");
        assert!(
            places[0].contains("made.txt")
                && places[0].contains("integrate_children")
                && (name.is_empty() || places[0].starts_with(&name)),
            "{places:?} ({coder})"
        );
        for elsewhere in ["missing.txt", "../made.txt", "/made.txt"] {
            assert!(agents.elsewhere(elsewhere).is_empty(), "{elsewhere}");
        }
        let applied = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(applied["status"], "applied", "{applied}");
        assert!(
            agents.elsewhere("made.txt").is_empty(),
            "an integrated coder's files are in the checkout"
        );
    }

    /// Two coders that changed different files merge, the project's check runs
    /// on the merged revision - it needs both files, which neither branch has
    /// alone - and only `apply` moves the user's branch, by a fast-forward.
    #[tokio::test]
    async fn integrate_children_merges_checks_and_fast_forwards() {
        let mut bench = IntegrationBench::new();
        let coder = bench.coder("first.txt=from the first coder").await;
        assert!(
            coder["text"]
                .as_str()
                .is_some_and(|text| text.contains("integrate_children")),
            "the parent is told how the work comes back: {coder}"
        );
        bench.coder("second.txt=from the second coder").await;
        let base = bench.head();

        // A failing check on the merged revision stops everything.
        bench.check("git show HEAD:first.txt HEAD:missing.txt");
        let failed = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(failed["status"], "checks_failed", "{failed}");
        assert!(failed["text"].as_str().unwrap().contains("missing.txt"));
        assert_eq!(bench.head(), base);
        assert_eq!(bench.status(), "");

        bench.check("git show HEAD:first.txt HEAD:second.txt");
        let ready = bench.integrate(serde_json::json!({})).await;
        assert_eq!(ready["status"], "ready", "{ready}");
        assert_eq!(bench.head(), base, "without apply the user's branch stays");
        assert_eq!(bench.read("first.txt"), None);

        let applied = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(applied["status"], "applied", "{applied}");
        assert_eq!(applied["previous_head"], base.as_str());
        assert_eq!(bench.head(), applied["final_commit"].as_str().unwrap());
        assert_eq!(
            bench.read("first.txt").as_deref(),
            Some("from the first coder\n")
        );
        assert_eq!(
            bench.read("second.txt").as_deref(),
            Some("from the second coder\n")
        );
        assert_eq!(
            bench.status(),
            "",
            "a fast-forward leaves nothing uncommitted"
        );
        let ancestry = Command::new("git")
            .args(["merge-base", "--is-ancestor", &base, "HEAD"])
            .current_dir(&bench.repo)
            .status()
            .expect("git starts");
        assert!(
            ancestry.success(),
            "the user's old head is in the new history"
        );

        let again = bench
            .tool
            .integrate(&serde_json::json!({}))
            .await
            .expect_err("integrated coders are not offered twice");
        assert!(again.message().contains("no finished coder"), "{again}");
    }

    /// Two coders that changed the same line conflict: the result names the
    /// file and the coder, and the user's checkout does not move.
    #[tokio::test]
    async fn integrate_children_reports_a_conflict_and_touches_nothing() {
        let bench = IntegrationBench::new();
        bench.coder("notes.txt=first version").await;
        let second = bench.coder("notes.txt=second version").await;
        let base = bench.head();
        let conflicted = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(conflicted["status"], "conflicted", "{conflicted}");
        let conflicts = conflicted["conflicts"].to_string();
        assert!(conflicts.contains("notes.txt"), "{conflicts}");
        assert!(
            conflicts.contains(second["name"].as_str().unwrap()),
            "the conflict names the coder that could not be merged: {conflicts}"
        );
        assert_eq!(bench.head(), base);
        assert_eq!(bench.read("notes.txt").as_deref(), Some("base\n"));
        assert_eq!(bench.status(), "");
    }

    /// A checkout the user changed while the coders worked is refused, before
    /// the merge and again at the moment of applying; the user's work stays.
    #[tokio::test]
    async fn integrate_children_refuses_a_checkout_the_user_changed() {
        let mut bench = IntegrationBench::new();
        bench.coder("first.txt=one").await;
        bench.coder("second.txt=two").await;
        let base = bench.head();

        std::fs::write(bench.repo.join("notes.txt"), "the user's edit\n").expect("user edit");
        let refused = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(refused["status"], "refused", "{refused}");
        assert_eq!(
            bench.read("notes.txt").as_deref(),
            Some("the user's edit\n")
        );
        assert_eq!(bench.head(), base);
        assert_eq!(bench.read("first.txt"), None);

        // The edit is undone byte for byte (a checkout could convert line
        // endings), then the user commits while the checks run: the recheck at
        // apply time catches what the first one could not.
        std::fs::write(bench.repo.join("notes.txt"), "base\n").expect("edit undone");
        let repo = bench.repo.to_string_lossy().replace('\\', "/");
        bench.check(&format!(
            "git -C \"{repo}\" commit --allow-empty -q -m during-integration"
        ));
        let late = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(late["status"], "refused", "{late}");
        assert_ne!(bench.head(), base, "the user's own commit landed");
        assert_eq!(
            bench.read("first.txt"),
            None,
            "no coder file reached the user"
        );
        assert_eq!(bench.read("notes.txt").as_deref(), Some("base\n"));
        assert_eq!(bench.status(), "");
    }

    #[tokio::test]
    async fn verification_regression_mutating_checks_cannot_certify_the_old_commit() {
        let mut bench = IntegrationBench::new();
        bench.coder("first.txt=coder version").await;
        let base = bench.head();
        bench.check("echo rewritten-during-check > first.txt");
        let result = bench.integrate(serde_json::json!({"apply": true})).await;
        assert_eq!(result["status"], "checks_failed", "{result}");
        assert_eq!(bench.head(), base);
        assert_eq!(bench.read("first.txt"), None);
        assert_eq!(bench.status(), "");
    }

    #[tokio::test]
    async fn verification_regression_canceled_integration_does_not_apply() {
        let bench = IntegrationBench::new();
        bench.coder("first.txt=coder version").await;
        let base = bench.head();
        bench.tool.parent_cancellation.cancel();
        let error = bench
            .tool
            .integrate(&serde_json::json!({"apply": true}))
            .await
            .expect_err("canceled");
        assert_eq!(error.code(), harness_types::ErrorCode::ProviderCanceled);
        assert_eq!(bench.head(), base);
        assert_eq!(bench.read("first.txt"), None);
    }

    #[test]
    fn integrate_children_arguments_are_bounded() {
        use super::IntegrateRequest;
        let parsed = IntegrateRequest::parse(&serde_json::json!({})).expect("defaults");
        assert!(parsed.children.is_none() && !parsed.apply);
        let parsed = IntegrateRequest::parse(
            &serde_json::json!({"children": ["coder-a", " b "], "apply": true}),
        )
        .expect("named");
        assert_eq!(
            parsed.children.as_deref(),
            Some(&["coder-a".to_owned(), "b".to_owned()][..])
        );
        assert!(parsed.apply);
        for wrong in [
            serde_json::json!({"apply": "yes"}),
            serde_json::json!({"children": "coder-a"}),
            serde_json::json!({"children": [""]}),
            serde_json::json!({"force": true}),
        ] {
            assert!(IntegrateRequest::parse(&wrong).is_err(), "{wrong}");
        }
    }

    /// `/subagent-model` and `/subagent-thinking` moved only the next child:
    /// the user switched a quota-bound model away and the running children
    /// stayed on it, their rows still naming it. A child that named its model
    /// keeps it; what it left to the defaults follows them.
    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "one scenario, told in order")]
    async fn running_children_follow_a_change_of_the_defaults() {
        use super::RlmChildren;
        use crate::interactive::repl::HostRequests;
        use serde_json::json;

        struct Defaults {
            model: std::sync::Mutex<String>,
            thinking: std::sync::Mutex<Option<harness_providers::ThinkingLevel>>,
        }
        impl ChildModels for Defaults {
            fn default_model(&self) -> Option<String> {
                self.model.lock().ok().map(|model| model.clone())
            }
            fn default_thinking(&self) -> Option<harness_providers::ThinkingLevel> {
                self.thinking.lock().ok().and_then(|thinking| *thinking)
            }
            fn resolve(&self, reference: &str) -> Result<ChildModel, String> {
                self.resolve_at(reference, None, false)
            }
            fn resolve_at(
                &self,
                reference: &str,
                level: Option<harness_providers::ThinkingLevel>,
                _strict: bool,
            ) -> Result<ChildModel, String> {
                Ok(ChildModel {
                    provider: Arc::new(harness_providers::MockProvider::text(reference)),
                    reference: reference.to_owned(),
                    price: None,
                    thinking: level.map(|level| level.as_str().to_owned()),
                })
            }
        }

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
            &(Arc::new(ScriptedBackend) as Arc<dyn harness_orchestrator::WorkerBackend>),
        );
        let defaults = Arc::new(Defaults {
            model: std::sync::Mutex::new("deepseek/flash".to_owned()),
            thinking: std::sync::Mutex::new(None),
        });
        let mut launch = launch_for(
            &repo,
            Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
        );
        launch.models = Some(Arc::clone(&defaults) as Arc<dyn ChildModels>);
        let children = RlmChildren {
            agents: Arc::clone(&agents),
            launch,
        };
        for kwargs in [
            json!({"name": "follower"}),
            json!({"name": "named", "model": "fixed/model"}),
            json!({"name": "pinned", "model": "fixed/model", "thinking": "high"}),
        ] {
            children
                .handle(&json!({"type": "rlm.run", "prompt": "slow survey", "kwargs": kwargs}))
                .await
                .expect("a known request type")
                .expect("spawn");
        }
        let row = |name: &str| {
            agents
                .view_rows()
                .into_iter()
                .find(|row| row.name == name)
                .map(|row| (row.model, row.thinking))
                .expect("the child's row")
        };
        assert_eq!(row("follower"), ("deepseek/flash".to_owned(), None));

        *defaults.model.lock().expect("model") = "openai-codex/luna".to_owned();
        *defaults.thinking.lock().expect("thinking") =
            Some(harness_providers::ThinkingLevel::Xhigh);
        let mut moved = agents.follow_defaults();
        moved.sort();
        assert_eq!(
            moved,
            vec![
                ("follower".to_owned(), "openai-codex/luna xhigh".to_owned()),
                ("named".to_owned(), "fixed/model xhigh".to_owned()),
            ],
            "the pinned child named both"
        );
        assert_eq!(
            row("follower"),
            ("openai-codex/luna".to_owned(), Some("xhigh".to_owned()))
        );
        assert_eq!(
            row("named"),
            ("fixed/model".to_owned(), Some("xhigh".to_owned()))
        );
        assert_eq!(
            row("pinned"),
            ("fixed/model".to_owned(), Some("high".to_owned()))
        );
        assert!(
            agents.follow_defaults().is_empty(),
            "nothing moves twice for one change"
        );

        // A child stopped before the change wakes on the model set after it.
        let follower = agents
            .shared
            .children
            .lock()
            .expect("children")
            .iter()
            .find(|child| child.name == "follower")
            .map(|child| child.task_id.clone())
            .expect("the follower");
        agents.shared.update(&follower, |child| {
            child.state = ChildState::Failed {
                error: "stopped by the user".to_owned(),
            };
        });
        *defaults.model.lock().expect("model") = "openai-codex/sol".to_owned();
        let moved = agents.follow_defaults();
        assert!(
            moved.contains(&("follower".to_owned(), "openai-codex/sol xhigh".to_owned())),
            "{moved:?}"
        );
        assert_eq!(
            agents
                .shared
                .launches
                .lock()
                .expect("launches")
                .get(&follower)
                .map(|launch| launch.model.reference.clone()),
            Some("openai-codex/sol".to_owned()),
            "the launch it wakes from names the new model"
        );
    }

    /// A running child's model is asked for at every call, so a swap reaches
    /// the next call of the turn already under way.
    #[tokio::test]
    async fn a_live_model_answers_with_the_model_set_last() {
        use harness_providers::ModelProvider as _;
        let model = |text: &str| ChildModel {
            provider: Arc::new(harness_providers::MockProvider::text(text)),
            reference: text.to_owned(),
            price: None,
            thinking: None,
        };
        let live = super::LiveModel::new(model("deepseek"));
        let answer = |live: &super::LiveModel| {
            live.stream(
                harness_providers::ProviderRequest::new(
                    harness_types::RequestId::generate(),
                    "m",
                    Vec::new(),
                ),
                CancellationToken::new(),
            )
        };
        let text = |events: Vec<harness_providers::ProviderStreamEvent>| format!("{events:?}");
        assert!(text(answer(&live).await.expect("first")).contains("deepseek"));
        live.set(model("luna"));
        let second = text(answer(&live).await.expect("second"));
        assert!(
            second.contains("luna") && !second.contains("deepseek"),
            "{second}"
        );
    }

    /// Three children ran at once and eight more waited, each drawn working
    /// with no call made; a twelfth was refused. Every child now runs at once.
    #[tokio::test]
    async fn every_child_runs_at_once() {
        use super::RlmChildren;
        use crate::interactive::repl::HostRequests;
        use serde_json::json;

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
            &(Arc::new(ScriptedBackend) as Arc<dyn harness_orchestrator::WorkerBackend>),
        );
        let children = RlmChildren {
            agents: Arc::clone(&agents),
            launch: launch_for(
                &repo,
                Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
            ),
        };
        for index in 0..12 {
            children
                .handle(&json!({"type": "rlm.run", "prompt": "slow survey", "kwargs": {"name": format!("c{index}")}}))
                .await
                .expect("a known request type")
                .expect("every spawn is admitted");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(agents.scheduler().live_worker_count(), 12, "all twelve run");
    }

    /// `[agents] max_workers` bounds how many of the root's children run at
    /// once; 0 (the default) bounds nothing, and a raised limit starts the
    /// ones waiting.
    #[tokio::test]
    async fn max_workers_holds_the_children_past_it() {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let shared = shared(sender);
        let unbounded = futures_util::future::join_all((0..20).map(|_| shared.worker_slot())).await;
        assert_eq!(unbounded.len(), 20);
        drop(unbounded);

        shared
            .max_workers
            .store(1, std::sync::atomic::Ordering::SeqCst);
        let first = shared.worker_slot().await;
        let waiting = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { shared.worker_slot().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished(), "the second waits for a place");
        drop(first);
        let second = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("it starts once the first ends")
            .expect("joined");

        let third = tokio::spawn({
            let shared = Arc::clone(&shared);
            async move { shared.worker_slot().await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!third.is_finished());
        shared
            .max_workers
            .store(0, std::sync::atomic::Ordering::SeqCst);
        shared.worker_freed.notify_waiters();
        tokio::time::timeout(Duration::from_secs(5), third)
            .await
            .expect("no limit starts it at once")
            .expect("joined");
        drop(second);
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
            &(Arc::new(ScriptedBackend) as Arc<dyn harness_orchestrator::WorkerBackend>),
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

        // prime-agent's runtime refuses the whole list for an activity kind it
        // does not know: a running child is waiting or executing, a settled one
        // has none.
        let listed = ask(json!({"type": "rlm.list_subagents"}))
            .await
            .expect("list");
        for row in listed["subagents"].as_array().expect("rows") {
            let activity = &row["activity"];
            assert!(
                activity.is_null()
                    || ["waiting", "writing", "executing"]
                        .contains(&activity["kind"].as_str().unwrap_or_default()),
                "{row}"
            );
        }

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
        assert_eq!(result["answer_text"], "answer: slow survey");
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

    /// A child answer longer than a notice holds is cut there, and the whole of
    /// it is kept in a file the cut names, so the parent can read the rest.
    #[tokio::test]
    async fn a_long_child_answer_is_kept_whole_in_a_file() {
        use super::RlmChildren;
        use crate::interactive::repl::HostRequests;
        use serde_json::json;

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
            &(Arc::new(ScriptedBackend) as Arc<dyn harness_orchestrator::WorkerBackend>),
        );
        let children = RlmChildren {
            agents,
            launch: launch_for(
                &repo,
                Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
            ),
        };
        let prompt = "x".repeat(8_100);
        let spawned = children
            .handle(&json!({"type": "rlm.run", "prompt": prompt, "kwargs": {"name": "long"}}))
            .await
            .expect("known")
            .expect("spawn");
        let collected = children
            .handle(&json!({"type": "rlm.collect", "targets": [spawned["rlm_child_id"]], "timeout_ms": 10_000}))
            .await
            .expect("known")
            .expect("collect");
        let preview = collected["results"][0]["answer_preview"]
            .as_str()
            .expect("an answer")
            .to_owned();
        let path = preview
            .rsplit_once("the whole answer is in ")
            .map(|(_, rest)| rest.trim_end_matches(']'))
            .expect("the cut names the file");
        assert_eq!(
            std::fs::read_to_string(path).expect("the whole answer"),
            format!("answer: {prompt}")
        );
    }

    /// prime-agent's `rlm.create_session` from the root: it needs a background
    /// worker, checks its arguments as prime-agent does, and hands the rest to
    /// the worker's session host with `cwd` read from the workspace.
    #[tokio::test]
    async fn rlm_create_session_goes_to_the_session_host() {
        use super::RlmChildren;
        use crate::interactive::repl::HostRequests;
        use serde_json::json;

        struct Host(std::sync::Mutex<Vec<crate::interactive::agents::CreateSession>>);
        impl crate::interactive::agents::SessionHost for Host {
            fn create_session(
                &self,
                request: crate::interactive::agents::CreateSession,
            ) -> Result<serde_json::Value, String> {
                let name = request.name.clone().unwrap_or_default();
                self.0.lock().expect("requests").push(request);
                Ok(json!({"active_session_id": "a1", "name": name}))
            }
        }

        let temporary = tempfile::tempdir().expect("temporary directory");
        let repo = temporary.path().join("repo");
        std::fs::create_dir_all(&repo).expect("repo directory");
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
            &(Arc::new(ScriptedBackend) as Arc<dyn harness_orchestrator::WorkerBackend>),
        );
        let children = RlmChildren {
            agents: Arc::clone(&agents),
            launch: launch_for(
                &repo,
                Arc::new(harness_providers::MockProvider::scripted(Vec::new())),
            ),
        };
        let ask = |request: serde_json::Value| {
            let children = &children;
            async move { children.handle(&request).await.expect("a known request") }
        };
        let request = json!({"type": "rlm.create_session", "prompt": "review", "kwargs": {"name": "rev", "cwd": "sub"}});
        assert_eq!(
            ask(request.clone()).await.expect_err("no worker"),
            "rlm.create_session requires a daemon-backed depth-0 session"
        );
        let host = Arc::new(Host(std::sync::Mutex::new(Vec::new())));
        agents.set_session_host(Arc::clone(&host) as Arc<dyn crate::interactive::agents::SessionHost>);
        let created = ask(request).await.expect("created");
        assert_eq!(created["active_session_id"], "a1");
        let seen = host.0.lock().expect("requests").clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].prompt, "review");
        assert_eq!(seen[0].name.as_deref(), Some("rev"));
        assert_eq!(seen[0].cwd.as_deref(), Some(repo.join("sub").as_path()));
        assert_eq!(
            ask(json!({"type": "rlm.create_session", "prompt": "x", "kwargs": {"color": "red", "age": 1}}))
                .await
                .expect_err("unknown kwargs"),
            "Unsupported rlm.create_session kwargs: age, color"
        );
        assert_eq!(
            ask(json!({"type": "rlm.create_session", "prompt": "  ", "kwargs": {}}))
                .await
                .expect_err("an empty prompt"),
            "rlm.create_session prompt must not be empty"
        );
        assert!(
            ask(json!({"type": "rlm.create_session", "prompt": "x", "kwargs": {"model": 3}}))
                .await
                .expect_err("a model that is not text")
                .contains("model must be a non-empty string")
        );
    }

    /// The brief is bounded in characters, as the schema's `maxLength` counts:
    /// a Vietnamese brief within it was refused for its bytes.
    #[test]
    fn a_brief_is_bounded_in_characters_not_bytes() {
        let vietnamese = "Bạn là coder. Làm phần máy chủ cho cột mốc M2. ".repeat(250);
        assert!(vietnamese.len() > 8192 && vietnamese.chars().count() <= 16_384);
        let (_, brief, _) = super::DelegateTool::arguments(
            &serde_json::json!({"role": "coder", "brief": vietnamese}),
        )
        .expect("a brief within the schema's maxLength");
        assert_eq!(brief.chars().count(), vietnamese.chars().count());
        let long = "ở".repeat(16_385);
        assert!(
            super::DelegateTool::arguments(&serde_json::json!({"role": "coder", "brief": long}))
                .is_err()
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
            16_384
        );
        assert_eq!(
            schema["parameters"]["properties"]["role"]["enum"],
            serde_json::json!(["explorer", "coder", "verifier"])
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

    use super::{ChildRecord, ChildState, DelegateHost, LedgerEntry, SessionAgents};
    use crate::interactive::events::SessionEvent;
    use crate::interactive::service::ChannelApprovalGate;
    use crate::interactive::store_lease::SharedStore;
    use harness_types::TaskId;

    /// One reply of a scripted model.
    #[derive(Clone)]
    enum Reply {
        Text(&'static str),
        Tool(&'static str, Value),
        /// A stream that fails, as a provider whose body cannot be decoded.
        Broken,
        /// A clean EOF without a terminal marker (empty or carrying text).
        Unterminated(&'static str),
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

        fn stream(
            &self,
            request: ProviderRequest,
            cancellation: CancellationToken,
        ) -> ProviderFuture {
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
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(ms)) => {}
                        () = cancellation.cancelled() => {
                            return Err(harness_providers::ProviderError::new(
                                harness_types::ErrorCode::ProviderCanceled,
                                "the provider request was canceled",
                            ));
                        }
                    }
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
                    Reply::Unterminated(text) => {
                        return Ok(vec![
                            ProviderStreamEvent::Started { request_id },
                            ProviderStreamEvent::text(text),
                        ]);
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
        let task_id = agents.shared.find(None, name).expect("child");
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

    /// A verifier judges with its own context and no editing tools - an edit it
    /// tries is denied, whatever the mode - and the harness reads its verdict.
    #[tokio::test]
    async fn a_verifier_cannot_edit_and_its_verdict_comes_back() {
        let bench = bench();
        let provider = Scripted::new(vec![
            Reply::Tool(
                "write_file",
                json!({"path": "a.txt", "content": "changed by the verifier\n"}),
            ),
            Reply::Text("a.txt:1 still says a\nVERDICT: FAIL"),
        ]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        let answer = host
            .verifier()
            .judge("judge whether a.txt says b".to_owned(), None)
            .await
            .expect("the verifier answers");
        assert_eq!(
            crate::interactive::verify::verdict(&answer),
            crate::interactive::verify::Verdict::Fail,
            "{answer}"
        );
        assert_eq!(
            std::fs::read_to_string(bench.repo.join("a.txt")).expect("file"),
            "a\n",
            "the verifier's edit was refused"
        );
        let seen = provider.seen.lock().expect("seen");
        let first = &seen[0];
        let tools = first
            .tool_schemas
            .iter()
            .filter_map(|schema| schema["function"]["name"].as_str())
            .collect::<Vec<_>>();
        assert!(tools.contains(&"read_file"), "{tools:?}");
        assert!(tools.contains(&"run_shell"), "{tools:?}");
        for editing in ["write_file", "edit_file", "apply_patch", "delegate"] {
            assert!(!tools.contains(&editing), "{editing} offered: {tools:?}");
        }
        let instructions = first
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            instructions.contains("independent verifier"),
            "{instructions}"
        );
    }

    /// A canceled harness-owned verification releases its running child.
    #[tokio::test]
    async fn cancelling_a_harness_verification_stops_its_child() {
        let bench = bench();
        let provider = Scripted::new(vec![Reply::Delay(
            5_000,
            Box::new(Reply::Text("VERDICT: PASS")),
        )]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        let verifier = host.verifier();
        let cancel = verifier.cancellation.clone();
        let judge =
            tokio::spawn(async move { verifier.judge("verify a.txt".to_owned(), None).await });
        tokio::time::timeout(Duration::from_secs(10), async {
            while provider.seen.lock().expect("seen").is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the verifier starts");
        cancel.cancel();
        assert!(judge.await.expect("judge task").is_err());
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            while bench.agents.running() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let _ = bench.agents.stop("all", "test cleanup");
        assert!(
            stopped.is_ok(),
            "a canceled verification must stop calling its model"
        );
    }

    #[tokio::test]
    async fn observation_returns_the_documented_family_and_agent_shapes() {
        let bench = bench();
        bench.agents.bind_ledger("observation-root");
        let host = host(&bench, Scripted::new(vec![Reply::Text("child answer")]));
        let spawned = spawn(&host, "observed", "read the fixture").await;
        state_of(&bench.agents, "observed").await;
        let requests = host.rlm_requests();
        let roster = requests
            .handle(&json!({"type": "agent_observe.list"}))
            .await
            .expect("known request")
            .expect("roster");
        assert_eq!(roster["current"]["sessionId"], "observation-root");
        let agents = roster["agents"].as_array().expect("family roster");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["relationship"], "child");
        assert_eq!(agents[0]["sessionName"], "observed");
        assert_eq!(agents[0]["sessionId"], spawned["rlm_child_id"]);
        assert_eq!(agents[0]["status"], "inactive");
        assert_eq!(agents[0]["isSessionActive"], false);
        let observed = requests
            .handle(&json!({"type": "agent_observe.get", "target": "observed"}))
            .await
            .expect("known request")
            .expect("agent summary");
        assert_eq!(observed["agent"], agents[0]);
        let id = spawned["rlm_child_id"].as_str().expect("child id");
        let by_suffix = requests
            .handle(&json!({"type": "agent_observe.get", "target": &id[id.len() - 8..]}))
            .await
            .expect("known request")
            .expect("unambiguous suffix");
        assert_eq!(by_suffix["agent"], agents[0]);
    }

    #[tokio::test]
    async fn observation_bounds_recent_messages_and_rejects_invalid_limits() {
        let bench = bench();
        let answer = Box::leak("🦊".repeat(150).into_boxed_str());
        let host = host(&bench, Scripted::new(vec![Reply::Text(answer)]));
        spawn(&host, "observed", "read the fixture").await;
        state_of(&bench.agents, "observed").await;
        let requests = host.rlm_requests();
        let recent = requests
            .handle(&json!({"type": "agent_observe.recent", "target": "observed", "limit": 1, "max_chars": 80}))
            .await
            .expect("known request")
            .expect("bounded messages");
        let messages = recent["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0]["text"]
                .as_str()
                .expect("preview")
                .chars()
                .count(),
            80
        );
        assert_eq!(messages[0]["role"], "assistant");
        for (field, bad) in [
            ("limit", json!(0)),
            ("limit", json!(51)),
            ("limit", json!(true)),
            ("max_chars", json!(79)),
            ("max_chars", json!(2001)),
        ] {
            let mut request = json!({"type": "agent_observe.recent", "target": "observed"});
            request[field] = bad;
            assert!(
                requests
                    .handle(&request)
                    .await
                    .expect("known request")
                    .is_err(),
                "{request}"
            );
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
        assert!(summary.contains("late (explorer)"), "{summary}");
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

    /// Ctrl-C stops every running child with the turn (`stop("all", ..)`), each
    /// reported as stopped, none left running.
    #[tokio::test]
    async fn ctrl_c_stops_every_running_child() {
        let mut bench = bench();
        let slow = || Reply::Delay(5_000, Box::new(Reply::Text("never")));
        let host = host(&bench, Scripted::new(vec![slow(), slow()]));
        spawn(&host, "first", "wait").await;
        spawn(&host, "second", "wait").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            bench
                .agents
                .stop("all", "Stopped by the user with Ctrl-C")
                .expect("stop"),
            2
        );
        for _ in 0..2 {
            match next_child_event(&mut bench.events).await {
                SessionEvent::ChildSettled { notice, .. } => {
                    assert!(
                        notice.contains("Stopped by the user with Ctrl-C"),
                        "{notice}"
                    );
                }
                other => panic!("expected a notice: {other:?}"),
            }
        }
        assert!(
            bench
                .agents
                .view_rows()
                .iter()
                .all(|row| row.status == "stopped"),
            "{:?}",
            bench.agents.view_rows()
        );
        // Nothing running: a second Ctrl-C finds nothing to stop, and that is fine.
        assert_eq!(bench.agents.stop("all", "again").expect("stop"), 0);
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
        // The agents view shows it stopped, not failed.
        let rows = bench.agents.view_rows();
        assert_eq!(
            rows.iter()
                .find(|row| row.name.contains("slow"))
                .map(|row| row.status.as_str()),
            Some("stopped"),
            "{rows:?}"
        );
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

    /// prime-agent: a message to a child that finished wakes it for a
    /// follow-up turn over its conversation so far.
    #[test]
    #[ignore = "subprocess barrier for a_message_after_the_final_inbox_poll_is_not_lost"]
    fn message_final_poll_hook_fixture() {
        use std::io::{Read, Write};
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .expect("hook input");
        let payload: Value = serde_json::from_str(&input).expect("hook payload");
        let marker = std::path::Path::new(payload["cwd"].as_str().expect("cwd"))
            .join("final-poll-hook.port");
        let Ok(port) = std::fs::read_to_string(marker) else {
            return;
        };
        let mut socket = std::net::TcpStream::connect(port.trim()).expect("hook barrier");
        socket
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("bound");
        socket.write_all(b"ready").expect("hook ready");
        let mut released = [0];
        socket.read_exact(&mut released).expect("hook release");
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // Keep the final-poll barrier and canceled-send assertions together.
    async fn a_message_after_the_final_inbox_poll_is_not_lost() {
        use sqlx::Connection;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let bench = bench();
        let parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("barrier");
        let marker = bench.repo.join("final-poll-hook.port");
        std::fs::write(&marker, listener.local_addr().expect("address").to_string())
            .expect("hook address");
        let provider = Scripted::new(vec![
            Reply::Tool("read_file", json!({"path": "a.txt"})),
            Reply::Text("first answer"),
            Reply::Text("after-late-message"),
        ]);
        let mut launch = super::tests::launch_for(&bench.repo, provider.clone());
        launch.hooks.push(harness_tools::ConfiguredToolHook {
            event: "subagent_stop".to_owned(),
            matcher: None,
            command: std::env::current_exe()
                .expect("test executable")
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "--exact".to_owned(),
                "interactive::delegation::real_worker_tests::message_final_poll_hook_fixture"
                    .to_owned(),
                "--ignored".to_owned(),
                "--nocapture".to_owned(),
            ],
            timeout_seconds: 20,
            source: "late message regression".to_owned(),
        });
        let host = DelegateHost::new(&bench.agents, launch, CancellationToken::new());
        spawn(&host, "worker", "read a.txt").await;
        let (mut socket, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
            .await
            .expect("the final poll finishes")
            .expect("hook connects");
        let mut ready = [0; 5];
        socket.read_exact(&mut ready).await.expect("hook ready");
        let receipt = host
            .rlm_requests()
            .handle(&json!({
                "type": "agent_message.send", "receiver_role": "child", "receiver_name": "worker",
                "message": "late-nonce-7b31: include this in the follow-up",
            }))
            .await
            .expect("known")
            .expect("accepted message");
        assert_eq!(receipt["receipts"][0]["deliveryStatus"], "queued");
        // Dropping a sender while SQLite is busy must release its admission
        // counter, so the receiver's cleanup cannot wait forever.
        let store = parent_lease.store();
        let options =
            sqlx::sqlite::SqliteConnectOptions::new().filename(&store.paths().database_path);
        let mut writer = sqlx::SqliteConnection::connect_with(&options)
            .await
            .expect("fixture writer");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut writer)
            .await
            .expect("hold writes");
        let task_id = bench.agents.shared.find(None, "worker").expect("child");
        let mut dropped_send = Box::pin(
            bench
                .agents
                .shared
                .deliver_to_child(&task_id, "dropped-before-enqueue-nonce".to_owned()),
        );
        assert!(futures_util::poll!(dropped_send.as_mut()).is_pending());
        assert_eq!(
            bench
                .agents
                .shared
                .update(&task_id, |child| child.in_flight_messages),
            Some(1)
        );
        drop(dropped_send);
        assert_eq!(
            bench
                .agents
                .shared
                .update(&task_id, |child| child.in_flight_messages),
            Some(0)
        );
        sqlx::query("ROLLBACK")
            .execute(&mut writer)
            .await
            .expect("release writes");
        std::fs::remove_file(&marker).expect("the follow-up does not block");
        socket
            .write_all(b"x")
            .await
            .expect("release the final hook");
        match state_of(&bench.agents, "worker").await {
            ChildState::Done { answer, .. } => assert_eq!(answer, "after-late-message"),
            other => panic!("late message follow-up failed: {other:?}"),
        }
        let seen = provider.seen.lock().expect("requests");
        let final_request = seen.last().expect("follow-up request");
        let deliveries = final_request
            .messages
            .iter()
            .filter(|message| {
                message.role == harness_providers::MessageRole::User
                    && message.content.contains("late-nonce-7b31")
            })
            .count();
        assert_eq!(deliveries, 1, "the accepted nonce reaches the model once");
        assert!(
            !final_request
                .messages
                .iter()
                .any(|message| message.content.contains("dropped-before-enqueue-nonce"))
        );
    }

    #[tokio::test]
    async fn a_message_wakes_a_finished_child() {
        let bench = bench();
        // The parent or a sibling keeps this writer open across the wake-up.
        // Closing it between runs would change the fence and hide a stale lease.
        let _parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let provider = Scripted::new(vec![
            Reply::Tool("read_file", json!({"path": "a.txt"})),
            Reply::Text("first answer"),
            Reply::Tool("read_file", json!({"path": "a.txt"})),
            Reply::Text("second answer"),
        ]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        spawn(&host, "worker", "look around").await;
        assert!(matches!(
            state_of(&bench.agents, "worker").await,
            ChildState::Done { .. }
        ));
        let task_id = bench.agents.shared.find(None, "worker").expect("child");
        let first_session = bench
            .agents
            .shared
            .update(&task_id, |child| {
                assert_eq!(child.tool_calls, 1, "the first run used a native tool");
                child.session_id.clone().expect("first session")
            })
            .expect("record");
        let receipts = host
            .rlm_requests()
            .handle(&json!({
                "type": "agent_message.send",
                "message": "one more thing",
                "receiver_role": "child",
                "receiver_name": "worker",
            }))
            .await
            .expect("known")
            .expect("send");
        assert_eq!(
            receipts["receipts"][0]["deliveryStatus"],
            json!("delivered")
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let answer = bench
                .agents
                .shared
                .update(&task_id, |child| match &child.state {
                    ChildState::Done { answer, .. } => Some(answer.clone()),
                    ChildState::Failed { error } => panic!("the woken child failed: {error}"),
                    _ => None,
                });
            if answer.flatten().as_deref() == Some("second answer") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the woken child never answered"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        bench.agents.shared.update(&task_id, |child| {
            assert_ne!(child.session_id.as_ref(), Some(&first_session));
            assert_eq!(child.tool_calls, 1, "the follow-up used a native tool");
        });
        let seen = provider.requests_text();
        assert!(
            seen.contains("first answer"),
            "the follow-up carries the history: {seen}"
        );
        assert!(seen.contains("one more thing"), "{seen}");
    }

    /// prime-agent's daemon announces a child's task once: a run a later
    /// message wakes it for ends without another no-reply notice. Each notice
    /// woke the parent, which asked the child again - a verifier waiting on CI
    /// and its parent pinged each other every half minute for hours.
    #[tokio::test]
    async fn a_woken_child_does_not_announce_its_task_again() {
        let mut bench = bench();
        let _parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let provider = Scripted::new(vec![
            Reply::Text("first answer"),
            Reply::Text("still waiting"),
        ]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        spawn(&host, "worker", "look around").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { notice, .. } => {
                assert!(
                    notice.contains("[child-exited: no-reply child:worker]"),
                    "{notice}"
                );
                assert!(notice.contains("first answer"), "{notice}");
            }
            other => panic!("expected the task's notice, got {other:?}"),
        }
        let task_id = bench.agents.shared.find(None, "worker").expect("child");
        host.rlm_requests()
            .handle(&json!({
                "type": "agent_message.send",
                "message": "any news?",
                "receiver_role": "child",
                "receiver_name": "worker",
            }))
            .await
            .expect("known")
            .expect("send");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let answered = bench
                .agents
                .shared
                .update(&task_id, |child| {
                    matches!(&child.state, ChildState::Done { answer, .. } if answer == "still waiting")
                })
                .unwrap_or(false);
            if answered {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the woken child never ran"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let again = tokio::time::timeout(
            Duration::from_millis(800),
            next_child_event(&mut bench.events),
        )
        .await;
        assert!(
            again.is_err(),
            "the woken run announced itself again: {again:?}"
        );
    }

    #[tokio::test]
    async fn wake_claims_messages_accepted_before_another_sender_wakes_the_child() {
        let bench = bench();
        let _parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let provider = Scripted::new(vec![
            Reply::Text("initial answer"),
            Reply::Text("answer after both messages"),
        ]);
        let host = host(&bench, provider.clone());
        spawn(&host, "worker", "look around").await;
        state_of(&bench.agents, "worker").await;
        let task_id = bench.agents.shared.find(None, "worker").expect("child");
        // The scheduler just published Done with a message accepted in the
        // dispatch/settle gap. A second sender wins the race to wake it.
        bench.agents.shared.update(&task_id, |child| {
            child.pending.push("accepted-gap-nonce-e32f".to_owned());
        });
        bench
            .agents
            .wake(&task_id, "competing-wake-nonce-b941".to_owned())
            .expect("wake");
        bench.agents.shared.update(&task_id, |child| {
            assert!(
                child.pending.is_empty(),
                "wake must claim earlier accepted messages atomically"
            );
            let wake = child.wake.as_deref().expect("wake prompt");
            assert_eq!(wake.matches("accepted-gap-nonce-e32f").count(), 1);
            assert_eq!(wake.matches("competing-wake-nonce-b941").count(), 1);
        });
        match state_of(&bench.agents, "worker").await {
            ChildState::Done { answer, .. } => assert_eq!(answer, "answer after both messages"),
            other => panic!("competing wake failed: {other:?}"),
        }
        let seen = provider.seen.lock().expect("requests");
        assert_eq!(seen.len(), 2, "both messages share the next turn");
        let request = seen.last().expect("follow-up request");
        for nonce in ["accepted-gap-nonce-e32f", "competing-wake-nonce-b941"] {
            assert_eq!(
                request
                    .messages
                    .iter()
                    .filter(|message| {
                        message.role == harness_providers::MessageRole::User
                            && message.content.contains(nonce)
                    })
                    .count(),
                1,
                "{nonce} reaches the model once"
            );
        }
    }

    #[tokio::test]
    async fn a_message_after_dispatch_returns_is_not_lost_at_settle() {
        use harness_orchestrator::{WorkerBackend, WorkerRequest};
        let bench = bench();
        let _parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let provider = Scripted::new(vec![
            Reply::Text("initial answer"),
            Reply::Tool("read_file", json!({"path": "a.txt"})),
            Reply::Text("pre-late-answer"),
            Reply::Text("after-settle-gap-message"),
        ]);
        let host = host(&bench, provider.clone());
        spawn(&host, "worker", "read a.txt").await;
        state_of(&bench.agents, "worker").await;
        let task_id = bench.agents.shared.find(None, "worker").expect("child");
        // Run the backend directly so this test owns the exact boundary between
        // its return and the scheduler publishing the child as settled.
        let request = bench
            .agents
            .shared
            .update(&task_id, |child| {
                child.state = ChildState::Running;
                child.wake = Some("read a.txt once more".to_owned());
                WorkerRequest {
                    task_id: task_id.clone(),
                    run_id: child.run_id.clone().expect("run id"),
                    generation: child.generation.into(),
                    depth: child.depth,
                    brief: child.brief.clone().expect("brief"),
                    worktree: child.worktree.clone(),
                    cancellation: child.cancellation.clone(),
                }
            })
            .expect("record");
        let backend = super::InteractiveWorkerBackend {
            shared: Arc::clone(&bench.agents.shared),
        };
        let outcome = backend.dispatch(request).await;
        assert!(matches!(
            outcome,
            harness_orchestrator::WorkerOutcome::Reported(_)
        ));
        let receipt = host
            .rlm_requests()
            .handle(&json!({
                "type": "agent_message.send", "receiver_role": "child", "receiver_name": "worker",
                "message": "settle-gap-nonce-c419: answer after this message",
            }))
            .await
            .expect("known")
            .expect("accepted message");
        assert_eq!(receipt["receipts"][0]["deliveryStatus"], "queued");
        bench.agents.shared.settle(&task_id, outcome);
        match state_of(&bench.agents, "worker").await {
            ChildState::Done { answer, .. } => assert_eq!(answer, "after-settle-gap-message"),
            other => panic!("settle gap follow-up failed: {other:?}"),
        }
        let seen = provider.seen.lock().expect("requests");
        let deliveries = seen
            .last()
            .expect("follow-up request")
            .messages
            .iter()
            .filter(|message| {
                message.role == harness_providers::MessageRole::User
                    && message.content.contains("settle-gap-nonce-c419")
            })
            .count();
        assert_eq!(deliveries, 1, "the queued nonce reaches the model once");
    }

    #[test]
    fn broadcast_sdk_shape_selects_all() {
        assert_eq!(
            super::message_arguments(&json!({"target": "all", "message": "hello"}))
                .expect("SDK broadcast"),
            ("hello".to_owned(), "all".to_owned(), None)
        );
    }

    #[test]
    fn message_receipts_identify_each_delivery_and_its_timestamp() {
        let receipts = (0..1000)
            .map(|_| super::receipt("worker", "root", "hello", "queued"))
            .collect::<Vec<_>>();
        let ids = receipts
            .iter()
            .map(|receipt| receipt["id"].as_str().expect("id"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            ids.len(),
            receipts.len(),
            "simultaneous receipts have unique ids"
        );
        assert!(
            receipts
                .iter()
                .all(|receipt| receipt["queuedAt"].as_u64().is_some())
        );
        let delivered = super::receipt("worker", "root", "hello", "delivered");
        assert!(delivered["deliveredAt"].as_u64().is_some());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // One family fixture verifies both broadcast directions and their boundaries.
    async fn broadcast_wakes_inactive_family_and_excludes_grandchildren_of_the_root() {
        let bench = bench();
        let _parent_lease = bench
            .agents
            .shared
            .store
            .lease()
            .await
            .expect("parent lease");
        let provider = Scripted::new(
            [
                vec![Reply::Text("initial"); 3],
                vec![Reply::Text("root broadcast read"); 3],
                vec![Reply::Text("native broadcast read"); 3],
            ]
            .concat(),
        );
        let host = host(&bench, provider.clone());
        for name in ["alpha", "beta", "gamma"] {
            spawn(&host, name, "wait for messages").await;
            state_of(&bench.agents, name).await;
        }
        let alpha = bench.agents.shared.find(None, "alpha").expect("alpha");
        let inner = TaskId::generate();
        let record = ChildRecord::from_ledger(LedgerEntry {
            task_id: inner.as_str().to_owned(),
            name: "inner".to_owned(),
            role: "explorer".to_owned(),
            model: "deepseek/fixture-model".to_owned(),
            thinking: None,
            depth: 2,
            max_depth: 2,
            parent: Some(alpha.as_str().to_owned()),
            status: "completed".to_owned(),
            answer: Some("inner initial".to_owned()),
            error: None,
            duration_ms: Some(1),
            tool_calls: 0,
            objective: "wait for messages".to_owned(),
            history: Vec::new(),
            worktree: None,
            integrated: None,
        })
        .expect("grandchild record");
        bench
            .agents
            .shared
            .children
            .lock()
            .expect("registry")
            .push(record);
        let launch = bench
            .agents
            .shared
            .launches
            .lock()
            .expect("launches")
            .get(&alpha)
            .expect("alpha launch")
            .clone();
        bench
            .agents
            .shared
            .launches
            .lock()
            .expect("launches")
            .insert(inner.clone(), launch);
        let receipt = host.rlm_requests().handle(&json!({
            "type": "agent_message.send", "receiver_role": "all", "message": "root-broadcast-nonce",
        })).await.expect("known").expect("root broadcast");
        assert_eq!(receipt["receipts"].as_array().expect("receipts").len(), 3);
        for name in ["alpha", "beta", "gamma"] {
            match state_of(&bench.agents, name).await {
                ChildState::Done { answer, .. } => assert_eq!(answer, "root broadcast read"),
                other => panic!("root broadcast failed: {other:?}"),
            }
        }
        bench.agents.shared.update(&inner, |child| {
            assert!(
                matches!(&child.state, ChildState::Done { answer, .. } if answer == "inner initial")
            );
        });
        let receipt = bench
            .agents
            .shared
            .send_from_child(
                &alpha,
                &json!({
                    "receiver_role": "all", "message": "native-broadcast-nonce",
                }),
            )
            .await
            .expect("native broadcast");
        assert_eq!(receipt["receipts"].as_array().expect("receipts").len(), 4);
        for name in ["beta", "gamma"] {
            match state_of(&bench.agents, name).await {
                ChildState::Done { answer, .. } => assert_eq!(answer, "native broadcast read"),
                other => panic!("native broadcast failed: {other:?}"),
            }
        }
        let _ = bench
            .agents
            .shared
            .wait(&inner, None, None)
            .await
            .expect("grandchild settles");
        bench.agents.shared.update(&inner, |child| {
            assert!(matches!(&child.state, ChildState::Done { answer, .. } if answer == "native broadcast read"));
        });
        let seen = provider.seen.lock().expect("requests");
        let inner_request = seen
            .iter()
            .find(|request| {
                request.messages.iter().any(|message| {
                    message.content.contains("native-broadcast-nonce")
                        && !message.content.contains("sibling:")
                        && message.content.contains("parent:alpha")
                })
            })
            .expect("own grandchild received its parent broadcast");
        assert!(
            !inner_request
                .messages
                .iter()
                .any(|message| message.content.contains("root-broadcast-nonce"))
        );
    }

    #[tokio::test]
    async fn a_message_wakes_a_child_stopped_after_native_tools() {
        for canceled in [false, true] {
            let bench = bench();
            let _parent_lease = bench
                .agents
                .shared
                .store
                .lease()
                .await
                .expect("parent lease");
            let mut replies = vec![Reply::Tool("read_file", json!({"path": "a.txt"}))];
            if canceled {
                replies.push(Reply::Delay(5_000, Box::new(Reply::Text("unused"))));
            } else {
                replies.extend(vec![Reply::Broken; 6]);
            }
            let provider = Scripted::new(replies);
            let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
            spawn(&host, "worker", "read a.txt").await;
            if canceled {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while provider.seen.lock().expect("seen").len() < 2 {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the native read completed before cancellation");
                let _ = bench.agents.stop("worker", "fixture cancellation");
            }
            let stopped = state_of(&bench.agents, "worker").await;
            assert!(
                if canceled {
                    matches!(stopped, ChildState::Cancelled { .. })
                } else {
                    matches!(stopped, ChildState::Failed { .. })
                },
                "{stopped:?}"
            );
            let task_id = bench.agents.shared.find(None, "worker").expect("child");
            bench.agents.shared.update(&task_id, |child| {
                assert_eq!(child.tool_calls, 1, "the stopped run used a native tool");
            });
            {
                let mut replies = provider.replies.lock().expect("replies");
                replies.clear();
                replies.extend([
                    Reply::Tool("read_file", json!({"path": "a.txt"})),
                    Reply::Text("recovered"),
                ]);
            }
            host.rlm_requests()
                .handle(&json!({
                    "type": "agent_message.send",
                    "message": "try again",
                    "receiver_role": "child",
                    "receiver_name": "worker",
                }))
                .await
                .expect("known")
                .expect("wake");
            match state_of(&bench.agents, "worker").await {
                ChildState::Done { answer, .. } => assert_eq!(answer, "recovered"),
                other => panic!("the stopped child failed to recover: {other:?}"),
            }
        }
    }

    /// A coder's commit is titled by what it says it did, not by its brief.
    #[test]
    fn a_coder_commit_is_titled_by_its_answer() {
        assert_eq!(
            super::commit_title(
                "## Xong

**Đã thêm** `src/cli.js` với 9 lệnh"
            )
            .as_deref(),
            Some("Xong")
        );
        assert_eq!(
            super::commit_title(
                "
- Added the parser and its tests"
            )
            .as_deref(),
            Some("Added the parser and its tests")
        );
        assert_eq!(
            super::commit_title(
                "---
***
"
            ),
            None
        );
        let long = "x".repeat(100);
        assert_eq!(
            super::commit_title(&long).map(|title| title.chars().count()),
            Some(73)
        );
    }

    /// The root's own `agent_message`: a child by name by default, every child
    /// with "all", and never a parent or sibling it does not have.
    #[test]
    fn the_root_messages_its_children_by_name() {
        let request = super::root_message_request(
            &json!({"message": "carry on", "receiver_name": "verifier-1"}),
        )
        .expect("a child by name");
        assert_eq!(request["receiver_role"], "child");
        assert_eq!(request["receiver_name"], "verifier-1");
        let all = super::root_message_request(&json!({"message": "stop", "receiver_role": "all"}))
            .expect("every child");
        assert_eq!(all["receiver_role"], "all");
        assert!(super::root_message_request(&json!({"message": "carry on"})).is_err());
        assert!(
            super::root_message_request(&json!({"message": "x", "receiver_role": "parent"}))
                .is_err()
        );
    }

    /// A conversation resumed in a new process: each child comes back as its
    /// own role, and the ones that ended unfinished are said to the parent -
    /// a stopped verifier, a coder cut off by the worker - while a finished one
    /// is not. A verifier comes back a verifier and can be woken read-only.
    #[tokio::test]
    async fn a_resumed_conversation_says_which_children_did_not_finish() {
        let bench = bench();
        let entry = |task: &str, name: &str, role: &str, status: &str, error: Option<&str>| {
            json!({
                "task_id": task,
                "name": name, "role": role, "model": "fixture/model", "depth": 1, "max_depth": 2,
                "parent": null, "status": status, "answer": null, "error": error,
                "duration_ms": 10, "tool_calls": 3, "objective": format!("{name} objective"), "history": []
            })
        };
        let verifier = TaskId::generate();
        let coder = TaskId::generate();
        let explorer = TaskId::generate();
        let ledger = json!([
            entry(
                verifier.as_str(),
                "verifier-e9c5",
                "verifier",
                "cancelled",
                Some("Stopped by the user with Ctrl-C")
            ),
            entry(coder.as_str(), "coder-9ea6", "coder", "running", None),
            entry(
                explorer.as_str(),
                "explorer-done",
                "explorer",
                "completed",
                None
            ),
        ]);
        let dir = bench
            .agents
            .shared
            .ledger_dir
            .get()
            .expect("ledger dir")
            .clone();
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("conversation-r.json"), ledger.to_string()).expect("ledger");
        bench.agents.bind_ledger("conversation-r");

        let role = |task: &TaskId| {
            let mut role = None;
            bench
                .agents
                .shared
                .update(task, |child| role = Some(child.role));
            role.expect("child")
        };
        assert_eq!(role(&verifier), super::AgentRole::Verifier);
        assert_eq!(role(&coder), super::AgentRole::Coder);
        let block = bench
            .agents
            .unfinished_block()
            .expect("unfinished children");
        assert!(
            block.contains("Do not claim one is still running"),
            "{block}"
        );
        assert!(
            block.contains("- verifier-e9c5 (verifier, stopped): Stopped by the user with Ctrl-C"),
            "{block}"
        );
        assert!(
            block.contains("- coder-9ea6 (coder, failed): the child was interrupted"),
            "{block}"
        );
        assert!(!block.contains("explorer-done"), "{block}");
    }

    /// prime-agent's spawn ledger: a conversation opened again lists its
    /// children, a running one as interrupted.
    #[tokio::test]
    async fn the_ledger_brings_a_conversations_children_back() {
        let bench = bench();
        let provider = Scripted::new(vec![Reply::Text("kept answer")]);
        let host = host(&bench, Arc::clone(&provider) as Arc<dyn ModelProvider>);
        bench.agents.bind_ledger("conversation-a");
        spawn(&host, "keeper", "remember this").await;
        assert!(matches!(
            state_of(&bench.agents, "keeper").await,
            ChildState::Done { .. }
        ));
        // Leaving the conversation closes its children; opening it again
        // brings them back from the ledger.
        bench.agents.reset();
        assert!(bench.agents.shared.find(None, "keeper").is_none());
        bench.agents.bind_ledger("conversation-a");
        let task_id = bench
            .agents
            .shared
            .find(None, "keeper")
            .expect("back from the ledger");
        let answer = bench
            .agents
            .shared
            .update(&task_id, |child| match &child.state {
                ChildState::Done { answer, .. } => answer.clone(),
                _ => String::new(),
            });
        assert_eq!(answer.as_deref(), Some("kept answer"));
        let entry = LedgerEntry {
            task_id: TaskId::generate().as_str().to_owned(),
            name: "gone".to_owned(),
            role: "explorer".to_owned(),
            model: "m".to_owned(),
            thinking: None,
            depth: 1,
            max_depth: 2,
            parent: None,
            status: "running".to_owned(),
            answer: None,
            error: None,
            duration_ms: Some(5),
            tool_calls: 0,
            objective: "x".to_owned(),
            history: Vec::new(),
            worktree: None,
            integrated: None,
        };
        let record = ChildRecord::from_ledger(entry).expect("record");
        assert!(
            matches!(record.state, ChildState::Failed { ref error } if error.contains("interrupted"))
        );
    }

    /// A coder's worktree outlives its worker: the ledger keeps it, a coder
    /// read back after a restart has it (and the commit it was integrated in,
    /// so it is never merged twice), and one whose worktree was removed since
    /// does not.
    #[test]
    fn a_coder_keeps_its_worktree_across_a_restart() {
        let directory = tempfile::tempdir().expect("dir");
        let task = TaskId::generate();
        let worktree = |path: &std::path::Path| {
            serde_json::from_value::<super::WorktreeRecord>(json!({
                "worktree_id": "wt-1", "task_id": task.as_str(), "run_id": harness_types::AgentRunId::generate().as_str(),
                "project_id": harness_types::ProjectId::generate().as_str(), "base_commit": "abc", "base_branch": "harness/base/1",
                "branch": "harness/p5/1", "path": path.display().to_string(), "write_scope": ["."],
                "state": "ready", "input_fingerprint": harness_types::ContentHash::from_bytes(b"x").as_str(),
                "result_fingerprint": null, "generation": 1
            }))
            .expect("worktree record")
        };
        let entry = |worktree: super::WorktreeRecord, integrated: Option<&str>| LedgerEntry {
            task_id: task.as_str().to_owned(),
            name: "coder-1".to_owned(),
            role: "coder".to_owned(),
            model: "m".to_owned(),
            thinking: None,
            depth: 1,
            max_depth: 2,
            parent: None,
            status: "cancelled".to_owned(),
            answer: None,
            error: Some("cut off".to_owned()),
            duration_ms: Some(5),
            tool_calls: 3,
            objective: "write the parser".to_owned(),
            history: Vec::new(),
            worktree: Some(worktree),
            integrated: integrated.map(str::to_owned),
        };
        let kept = ChildRecord::from_ledger(entry(worktree(directory.path()), Some("c0ffee")))
            .expect("record");
        assert_eq!(kept.role, super::AgentRole::Coder);
        assert!(kept.worktree.is_some(), "the worktree comes back");
        assert_eq!(kept.integrated.as_deref(), Some("c0ffee"));
        let written = serde_json::to_value(kept.ledger_entry()).expect("entry");
        assert_eq!(written["integrated"], "c0ffee");
        assert_eq!(written["worktree"]["branch"], "harness/p5/1");
        // The brief it is woken with passes the coder's grant check.
        let issued = kept.worktree.as_ref().expect("worktree");
        let brief = super::coder_brief(&task, &kept.objective, issued).expect("brief");
        assert_eq!(brief.grants.project_id, issued.project_id);
        assert_eq!(brief.grants.write_scope, issued.write_scope);
        assert!(brief.grants.edit_workspace);
        assert_eq!(brief.role, super::AgentRole::Coder);
        let gone =
            ChildRecord::from_ledger(entry(worktree(&directory.path().join("removed")), None))
                .expect("record");
        assert!(
            gone.worktree.is_none(),
            "a removed worktree is not brought back"
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
        let task_id = bench.agents.shared.find(None, "noter").expect("child");
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

    /// Answers by what a request is about, so children running side by side
    /// get their own replies whatever order their requests arrive in.
    struct Routed {
        route: Box<dyn Fn(&ProviderRequest) -> Reply + Send + Sync>,
        seen: Mutex<Vec<ProviderRequest>>,
    }

    impl Routed {
        fn new(route: impl Fn(&ProviderRequest) -> Reply + Send + Sync + 'static) -> Arc<Self> {
            Arc::new(Self {
                route: Box::new(route),
                seen: Mutex::new(Vec::new()),
            })
        }

        /// The requests of the child whose brief is `brief`.
        fn requests_of(&self, brief: &str) -> Vec<ProviderRequest> {
            self.seen
                .lock()
                .expect("seen")
                .iter()
                .filter(|request| is_child(request, brief))
                .cloned()
                .collect()
        }
    }

    fn is_child(request: &ProviderRequest, brief: &str) -> bool {
        request
            .messages
            .iter()
            .any(|message| message.content.contains(&format!("Brief:\n{brief}")))
    }

    fn offers(request: &ProviderRequest, tool: &str) -> bool {
        request
            .tool_schemas
            .iter()
            .any(|schema| schema["function"]["name"] == tool)
    }

    fn last_text(request: &ProviderRequest) -> String {
        request
            .messages
            .last()
            .map(|message| message.content.clone())
            .unwrap_or_default()
    }

    fn tool_results(request: &ProviderRequest) -> usize {
        request
            .messages
            .iter()
            .filter(|message| message.role == harness_providers::MessageRole::Tool)
            .count()
    }

    impl ModelProvider for Routed {
        fn capabilities(&self) -> ModelCapabilities {
            ModelCapabilities::deepseek_fixture()
        }

        fn stream(&self, request: ProviderRequest, cancel: CancellationToken) -> ProviderFuture {
            let reply = (self.route)(&request);
            self.seen.lock().expect("seen").push(request.clone());
            let scripted = Scripted::new(vec![reply]);
            scripted.stream(request, cancel)
        }
    }

    /// prime-agent's nesting: with prime-agent's default depth of 2 the root's
    /// child spawns a child of its own, whose answer comes back to the child -
    /// not to the root - and the root hears only from its own child.
    #[tokio::test]
    async fn a_child_spawns_a_grandchild_and_hears_its_answer() {
        let mut bench = bench();
        bench.agents.set_max_depth(2, super::MaxDepthSource::Chat);
        let provider = Routed::new(|request| {
            if is_child(request, "inner task") {
                Reply::Text("inner answer")
            } else if tool_results(request) == 0 {
                Reply::Tool(
                    "delegate",
                    json!({"role": "explorer", "brief": "inner task"}),
                )
            } else {
                Reply::Text("outer answer")
            }
        });
        let host = host(&bench, provider.clone());
        spawn(&host, "outer", "outer task").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { name, notice } => {
                assert_eq!(name, "outer", "the root hears only from its own child");
                assert!(notice.contains("outer answer"), "{notice}");
            }
            other => panic!("expected the child's notice: {other:?}"),
        }
        let outer = provider.requests_of("outer task");
        assert!(offers(&outer[0], "delegate"), "depth 1 of 2 may spawn");
        assert!(
            last_text(outer.last().expect("outer requests")).contains("inner answer"),
            "the grandchild's answer is the delegate result"
        );
        let inner = provider.requests_of("inner task");
        assert!(!offers(&inner[0], "delegate"), "depth 2 of 2 may not spawn");
        let task_id = bench
            .agents
            .shared
            .children
            .lock()
            .expect("children")
            .iter()
            .find(|child| child.depth == 2)
            .map(|child| (child.parent.clone(), child.task_id.clone()));
        let (parent, _) = task_id.expect("the grandchild is recorded");
        assert_eq!(parent, bench.agents.shared.find(None, "outer"));
        assert!(
            bench
                .agents
                .summary()
                .iter()
                .any(|line| line.starts_with("  ")),
            "/agents shows the grandchild under its parent"
        );
    }

    /// A child whose own child runs past its turn is not finished: it waits,
    /// reads the grandchild's notice in one more turn, and only then reports to
    /// the root (prime-agent's `waitForRlmQuiescence`).
    #[tokio::test]
    async fn a_child_waits_for_its_subtree_before_it_reports() {
        let mut bench = bench();
        let provider = Routed::new(|request| {
            if is_child(request, "inner task") {
                Reply::Delay(400, Box::new(Reply::Text("inner late")))
            } else if last_text(request).contains("[child-exited") {
                Reply::Text("outer final")
            } else if tool_results(request) == 0 {
                Reply::Tool(
                    "delegate",
                    json!({"role": "explorer", "brief": "inner task", "wait": false}),
                )
            } else {
                Reply::Text("outer first")
            }
        });
        let host = host(&bench, provider.clone());
        spawn(&host, "outer", "outer task").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { name, notice } => {
                assert_eq!(name, "outer");
                assert!(
                    notice.contains("outer final"),
                    "the child reported after its subtree: {notice}"
                );
            }
            other => panic!("expected the child's notice: {other:?}"),
        }
        let outer = provider.requests_of("outer task");
        let last = last_text(outer.last().expect("outer requests"));
        assert!(
            last.contains("[child-exited: no-reply child:") && last.contains("inner late"),
            "the grandchild's notice opened the child's last turn: {last}"
        );
    }

    /// No earlier terminal answer means a failed stream has nothing to salvage.
    #[tokio::test]
    async fn a_failed_first_turn_has_no_partial_answer() {
        for failure in [
            Reply::Broken,
            Reply::Unterminated(""),
            Reply::Unterminated("cut off"),
        ] {
            let mut bench = bench();
            let provider = Routed::new(move |_| failure.clone());
            let host = host(&bench, provider.clone());
            spawn(&host, "first", "first task").await;
            let state = state_of(&bench.agents, "first").await;
            assert!(matches!(state, ChildState::Failed { .. }));
            let collected = host
                .rlm_requests()
                .handle(&json!({
                    "type": "rlm.collect", "targets": ["first"], "timeout_ms": 0
                }))
                .await
                .expect("known")
                .expect("collect");
            let result = &collected["results"][0];
            assert_eq!(result["status"], "error");
            assert_eq!(result["settled"], true);
            assert_eq!(result["partial"], false);
            assert!(result["answer_preview"].is_null());
            assert!(
                result["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty())
            );
            match next_child_event(&mut bench.events).await {
                SessionEvent::ChildSettled { notice, .. } => {
                    assert!(notice.starts_with("[child-failed child:first]"), "{notice}");
                    assert!(!notice.contains("Last verified answer"), "{notice}");
                }
                other => panic!("expected failure notice: {other:?}"),
            }
            assert!(provider.requests_of("first task").len() <= 3);
        }
    }

    /// A verified answer remains collectable if the subtree follow-up fails.
    /// The failed follow-up's text is not promoted to a verified answer.
    #[tokio::test]
    async fn a_failed_follow_up_keeps_the_last_verified_answer_as_partial() {
        for failure in [
            Reply::Broken,
            Reply::Unterminated(""),
            Reply::Unterminated("cut off"),
        ] {
            let mut bench = bench();
            let provider = Routed::new(move |request| {
                if is_child(request, "inner task") {
                    Reply::Delay(400, Box::new(Reply::Text("inner late")))
                } else if last_text(request).contains("[child-exited") {
                    failure.clone()
                } else if tool_results(request) == 0 {
                    Reply::Tool(
                        "delegate",
                        json!({"role": "explorer", "brief": "inner task", "wait": false}),
                    )
                } else {
                    Reply::Text("outer verified answer")
                }
            });
            let host = host(&bench, provider.clone());
            spawn(&host, "outer", "outer task").await;
            let state = state_of(&bench.agents, "outer").await;
            let payload =
                super::state_payload(harness_orchestrator::AgentRole::Explorer, "outer", &state);
            assert_eq!(payload["status"], "failed", "{payload}");
            assert_eq!(payload["partial"], true, "{payload}");
            assert_eq!(payload["partial_answer"], "outer verified answer");
            let text = payload["text"].as_str().expect("model-visible text");
            assert!(text.starts_with("[child-failed child:outer]"), "{text}");
            assert!(
                text.contains("Last verified answer (partial): outer verified answer"),
                "{text}"
            );
            assert!(
                payload["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty())
            );
            assert_eq!(payload["tool_calls"], 1);
            for _ in 0..2 {
                let collected = host
                    .rlm_requests()
                    .handle(&json!({
                        "type": "rlm.collect", "targets": ["outer"], "timeout_ms": 0
                    }))
                    .await
                    .expect("known")
                    .expect("collect");
                let result = &collected["results"][0];
                assert_eq!(result["status"], "error");
                assert_eq!(result["settled"], true);
                assert_eq!(result["partial"], true);
                assert_eq!(result["answer_preview"], "outer verified answer");
                assert_eq!(result["error"], payload["error"]);
            }
            let listed = host
                .rlm_requests()
                .handle(&json!({"type": "rlm.list_subagents"}))
                .await
                .expect("known")
                .expect("list");
            let outer = listed["subagents"]
                .as_array()
                .expect("rows")
                .iter()
                .find(|row| row["session_name"] == "outer")
                .expect("outer");
            assert_eq!(outer["status"], "error");
            assert_eq!(outer["partial"], true);
            assert_eq!(outer["answer_preview"], "outer verified answer");
            match next_child_event(&mut bench.events).await {
                SessionEvent::ChildSettled { notice, .. } => {
                    assert!(notice.starts_with("[child-failed child:outer]"), "{notice}");
                    assert!(
                        notice.contains("Last verified answer (partial): outer verified answer"),
                        "{notice}"
                    );
                    assert!(!notice.contains("partial): cut off"), "{notice}");
                }
                other => panic!("expected failure notice: {other:?}"),
            }
            assert!(
                provider.requests_of("outer task").len() <= 5,
                "retries are bounded"
            );
        }
    }

    /// With no limit, the default, a grandchild spawns a child of its own: its
    /// scheduler is made when the first child reaches depth 3 (two levels were
    /// all a session had), and each answer comes back one level up.
    #[tokio::test]
    async fn with_no_limit_children_nest_past_two_levels() {
        let mut bench = bench();
        assert_eq!(
            bench.agents.max_depth(),
            (
                super::UNLIMITED_RLM_MAX_DEPTH,
                super::MaxDepthSource::Default
            )
        );
        let provider = Routed::new(|request| {
            if is_child(request, "deep task") {
                Reply::Text("deep answer")
            } else if is_child(request, "middle task") {
                if tool_results(request) == 0 {
                    Reply::Tool(
                        "delegate",
                        json!({"role": "explorer", "brief": "deep task"}),
                    )
                } else {
                    Reply::Text("middle answer")
                }
            } else if tool_results(request) == 0 {
                Reply::Tool(
                    "delegate",
                    json!({"role": "explorer", "brief": "middle task"}),
                )
            } else {
                Reply::Text("outer answer")
            }
        });
        let host = host(&bench, provider.clone());
        spawn(&host, "outer", "outer task").await;
        match next_child_event(&mut bench.events).await {
            SessionEvent::ChildSettled { name, notice } => {
                assert_eq!(name, "outer", "the root hears only from its own child");
                assert!(notice.contains("outer answer"), "{notice}");
            }
            other => panic!("expected the child's notice: {other:?}"),
        }
        let middle = provider.requests_of("middle task");
        assert!(
            last_text(middle.last().expect("middle requests")).contains("deep answer"),
            "the depth-3 answer is the middle child's delegate result"
        );
        let deep = provider.requests_of("deep task");
        assert!(
            offers(&deep[0], "delegate"),
            "with no limit depth 3 may spawn"
        );
        let depths = bench
            .agents
            .shared
            .children
            .lock()
            .expect("children")
            .iter()
            .map(|child| child.depth)
            .collect::<Vec<_>>();
        assert!(depths.contains(&3), "{depths:?}");
        assert_eq!(bench.agents.levels.lock().expect("levels").len(), 3);
    }

    #[test]
    fn a_max_depth_is_a_number_or_unlimited() {
        assert_eq!(super::parse_max_depth("3"), Some(3));
        assert_eq!(super::parse_max_depth(" 0 "), Some(0));
        for unlimited in ["unlimited", "Unlimited", "none", "inf", "-1"] {
            assert_eq!(
                super::parse_max_depth(unlimited),
                Some(super::UNLIMITED_RLM_MAX_DEPTH),
                "{unlimited}"
            );
        }
        assert_eq!(super::parse_max_depth("deep"), None);
        assert_eq!(
            super::describe_max_depth(super::UNLIMITED_RLM_MAX_DEPTH),
            "unlimited"
        );
        assert_eq!(super::describe_max_depth(2), "2");
    }

    /// `RLM_MAX_DEPTH` 0 stops the root from spawning, with prime-agent's error;
    /// 1 keeps the root's children from spawning.
    #[tokio::test]
    async fn the_max_depth_bounds_who_may_spawn() {
        let bench = bench();
        let provider = Routed::new(|_| Reply::Text("leaf"));
        let host = host(&bench, provider.clone());
        bench.agents.set_max_depth(0, super::MaxDepthSource::Chat);
        let refused = host
            .rlm_requests()
            .handle(&json!({"type": "rlm.run", "prompt": "t", "kwargs": {"name": "x"}}))
            .await
            .expect("known request")
            .expect_err("depth 0 spawns nothing");
        assert_eq!(
            refused,
            "RLM recursion depth limit reached (RLM_DEPTH=0, RLM_MAX_DEPTH=0)"
        );
        bench.agents.set_max_depth(1, super::MaxDepthSource::Chat);
        spawn(&host, "only", "only task").await;
        assert!(matches!(
            state_of(&bench.agents, "only").await,
            ChildState::Done { .. }
        ));
        let only = provider.requests_of("only task");
        assert!(!offers(&only[0], "delegate"), "depth 1 of 1 may not spawn");
    }
}
