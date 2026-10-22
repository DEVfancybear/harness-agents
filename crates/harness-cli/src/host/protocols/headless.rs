//! Headless one-turn launch.
//!
//! A headless turn runs through the same application service the interactive app
//! uses: same runtime, same store, same tool gate. Results go to stdout, logs stay
//! on stderr, and no raw terminal mode is ever enabled. An unconfigured provider is
//! reported as an actionable error instead of a fabricated answer.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use harness_providers::{CancellationToken, MockProvider, ModelCapabilities, ModelProvider};

use super::events::SessionEvent;
use harness_runtime::{
    BudgetLedger, EvidenceKind, GoalCriterion, GoalSpec, HumanInputService, RunInbox, RunRequest,
    RuntimeConfig, RuntimeService,
};
use harness_store_sqlite::{SqliteStore, StoreError};
use harness_tools::{
    ApprovalMode, ToolExecutionService, ToolOutput, TurnDriver, TurnObserver, TurnOptions,
    TurnOutcome, TurnProgress, observe_workspace,
};
use harness_types::{
    AcceptanceCommand, AcceptanceRecord, BudgetId, ContentHash, CriterionEvidence, CriterionState,
    CriterionStatus, ErrorCode, HarnessError, InputId, RuntimeCommandId, SessionId, TaskId,
    ToolOutcomeState,
};

use super::HeadlessOptions;
use super::attachments;
use super::bootstrap::{self, LaunchContext, LaunchRequest};
use super::bounds;
use super::config::ConfigOverrides;
use super::extensions;
use super::paths::{HostPlatform, LaunchEnvironment};
use super::project;
use super::service::{
    EnvironmentCredential, resolve_provider_with_overrides, validate_credential_file,
};

/// A validated single-turn headless request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HeadlessRequest {
    pub prompt: String,
    pub json: bool,
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
    pub options: HeadlessOptions,
}

/// Machine output stays line-oriented and contains no terminal control codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum OutputFormat {
    Text,
    Json,
    StreamJson,
}

impl OutputFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            "stream-json" => Some(Self::StreamJson),
            _ => None,
        }
    }
}

/// Where a headless run's lines go: this process's stdout and stderr, or - for
/// a run a background worker carries out - the terminal that asked for it.
pub trait Output: Send + Sync {
    fn stdout(&self, line: &str);
    fn stderr(&self, line: &str);
}

/// This process's own stdout and stderr.
pub struct Console;

impl Output for Console {
    fn stdout(&self, line: &str) {
        println!("{line}");
    }

    fn stderr(&self, line: &str) {
        eprintln!("{line}");
    }
}

/// The store a headless run writes through.
pub enum RunStore {
    /// Open the project's writer for this run and close it afterwards.
    Own,
    /// The store a background worker shares with its agents.
    Shared(Arc<super::store_lease::SharedStore>),
}

/// Auto-allowed actions remain visible in headless JSON and plain output.
struct HeadlessObserver {
    auto_allowed: Mutex<Vec<String>>,
    notices: Mutex<Vec<String>>,
    stream_json: bool,
    approval_blocked: AtomicBool,
    out: Arc<dyn Output>,
    /// Prompt and completion tokens so far: an autonomous run's token budget.
    tokens: AtomicU64,
    /// The conversation, for the diagnostics log.
    task: String,
}

impl HeadlessObserver {
    fn stream_event(&self, kind: &str, fields: &serde_json::Value) {
        if self.stream_json {
            self.out
                .stdout(&serde_json::json!({"type": kind, "data": fields}).to_string());
        }
    }
}

impl TurnObserver for HeadlessObserver {
    fn observe(&self, progress: TurnProgress) {
        let record = match &progress {
            TurnProgress::Notice(message) => {
                Some(harness_types::diag::info("notice", "shown").msg(message.clone()))
            }
            TurnProgress::StreamRestarted => {
                Some(harness_types::diag::warn("provider", "stream_restarted"))
            }
            TurnProgress::ToolSettled {
                name,
                call_id,
                ok: false,
                detail,
            } => Some(
                harness_types::diag::warn("tool", "failed")
                    .msg(detail.clone().unwrap_or_default())
                    .call(call_id)
                    .field("tool", name.clone()),
            ),
            _ => None,
        };
        if let Some(record) = record {
            record.task(&self.task).emit();
        }
        match &progress {
            TurnProgress::TextDelta(text) => {
                self.stream_event("text.delta", &serde_json::json!({"text": text}));
            }
            TurnProgress::ThinkingDelta(text) => {
                self.stream_event("thinking.delta", &serde_json::json!({"text": text}));
            }
            TurnProgress::StreamRestarted => {
                self.stream_event("stream.restarted", &serde_json::json!({}));
            }
            TurnProgress::ToolProgress { name, text, .. } => self.stream_event(
                "tool.progress",
                &serde_json::json!({"name": name, "text": text}),
            ),
            TurnProgress::Usage {
                prompt_tokens,
                completion_tokens,
                ..
            } => {
                self.tokens.fetch_add(
                    prompt_tokens.saturating_add(*completion_tokens),
                    Ordering::SeqCst,
                );
                self.stream_event("usage", &serde_json::json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens}));
            }
            TurnProgress::ToolStarted { name, summary, .. } => self.stream_event(
                "tool.started",
                &serde_json::json!({"name": name, "summary": summary}),
            ),
            TurnProgress::ToolSettled {
                name, ok, detail, ..
            } => {
                self.stream_event(
                    "tool.settled",
                    &serde_json::json!({"name": name, "ok": ok, "detail": detail}),
                );
                if !ok
                    && detail.as_deref().is_some_and(|text| {
                        text.contains("approval") || text.contains("denied by the user")
                    })
                {
                    self.approval_blocked.store(true, Ordering::SeqCst);
                    self.stream_event(
                        "approval.blocked",
                        &serde_json::json!({"name": name, "reason": detail}),
                    );
                }
            }
            _ => {}
        }
        match progress {
            TurnProgress::Info(message) => {
                if let Ok(mut messages) = self.auto_allowed.lock() {
                    messages.push(message);
                }
            }
            TurnProgress::Notice(message) => {
                if let Ok(mut messages) = self.notices.lock() {
                    messages.push(message);
                }
            }
            _ => {}
        }
    }
}

/// Debug-only acceptance trace for a child that is killed at a hard deadline.
/// Normal users never see it, and release artifacts do not contain the switch.
fn acceptance_trace(#[cfg_attr(not(debug_assertions), allow(unused_variables))] stage: &str) {
    #[cfg(debug_assertions)]
    if std::env::var_os("HA_TEST_TRACE_HEADLESS").is_some() {
        eprintln!("HA_HEADLESS_PHASE {stage}");
    }
}

/// Parse the `--criteria` values into typed evidence kinds.
///
/// An unknown kind is refused by name: a criterion the host cannot check must
/// not silently disappear from the goal.
fn parse_criteria(values: &[String]) -> Result<Vec<GoalCriterion>, HarnessError> {
    let mut criteria = Vec::new();
    for (index, value) in values.iter().enumerate() {
        let kind = match value.trim().to_lowercase().as_str() {
            "response" => EvidenceKind::Response,
            "tool_execution" | "tool" => EvidenceKind::ToolExecution,
            "file_change" | "file" => EvidenceKind::FileChange,
            "check" | "test" => EvidenceKind::Check,
            "artifact" => EvidenceKind::Artifact,
            other => {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    format!(
                        "unknown --criteria kind {other:?}; use response, tool_execution, file_change, check or artifact"
                    ),
                ));
            }
        };
        criteria.push(GoalCriterion::required(
            format!("criterion-{}", index + 1),
            kind,
        ));
    }
    if criteria.is_empty() {
        criteria.push(GoalCriterion::required(
            "criterion-1",
            EvidenceKind::Response,
        ));
    }
    Ok(criteria)
}

/// The durable run state, read back from the store for the JSON report.
async fn run_state_label(store: &SqliteStore, outcome: &harness_tools::TurnOutcome) -> String {
    store
        .run_by_id(&outcome.run_id)
        .await
        .ok()
        .flatten()
        .map_or_else(|| "unknown".to_owned(), |run| run.state.as_str().to_owned())
}

/// Run one headless turn.
///
/// Run one bounded headless turn and report it as JSON.
///
/// The body is a linear sequence: resolve, run exactly one turn, shut down. It is
/// long because it wires real components, not because it branches.
///
/// As prime-agent runs a headless session through its daemon, the turn runs in
/// the project's background worker, which shares the project store with the
/// agents it runs: an agent busy in a turn no longer leaves `ha exec` waiting
/// for the store's writer. `HA_DAEMON=off`, or a worker that cannot be reached,
/// runs the turn in this process as before.
pub async fn run(request: HeadlessRequest) -> Result<ExitCode, HarnessError> {
    acceptance_trace("start");
    let caller_dir = std::env::current_dir().map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("the current working directory could not be resolved: {error}"),
        )
    })?;
    let environment = LaunchEnvironment::capture();
    let context = bootstrap::resolve(LaunchRequest {
        cwd: request.cwd.clone(),
        caller_dir: caller_dir.clone(),
        platform: HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    })?;
    acceptance_trace("bootstrap_resolved");
    // An unconfigured provider fails here, before a worker is started: a run
    // that cannot reach a model must not leave state behind.
    if !request.options.mock {
        resolve_provider_with_overrides(
            &context.paths.config_file,
            &context.project.root,
            &environment,
            &context.paths.data_dir,
            &ConfigOverrides {
                approval: request.options.approval.clone(),
                allowed_tools: request.options.allowed_tools.clone(),
                disallowed_tools: request.options.disallowed_tools.clone(),
                model: request.options.model.clone(),
                ..ConfigOverrides::default()
            },
        )
        .map_err(|message| HarnessError::new(ErrorCode::ServiceUnavailable, message))?;
        validate_credential_file(&environment, &context.paths.data_dir)
            .map_err(|message| HarnessError::new(ErrorCode::SecretNotGranted, message))?;
    }
    if super::agents::enabled(&environment, &context.paths.config_file) {
        match super::agents::client::exec(&context, &environment, &request, caller_dir).await {
            Ok(code) => return Ok(ExitCode::from(code)),
            Err(super::agents::client::ExecError::Failed(error)) => return Err(error),
            Err(super::agents::client::ExecError::Unavailable(reason)) => {
                eprintln!("ha: {reason}; this process runs the turn");
            }
        }
    }
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    let signal_listener = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let result = run_with(
        request,
        &context,
        &environment,
        RunStore::Own,
        Arc::new(Console),
        cancellation,
    )
    .await;
    signal_listener.abort();
    result.map(ExitCode::from)
}

/// Run one bounded headless turn through `run_store`, writing its lines to
/// `out`; the exit code is the result.
///
/// The body is a linear sequence: resolve, run exactly one turn, shut down. It is
/// long because it wires real components, not because it branches.
///
/// # Errors
/// The provider, the store or the turn failed.
#[allow(clippy::too_many_lines)]
pub async fn run_with(
    request: HeadlessRequest,
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    run_store: RunStore,
    out: Arc<dyn Output>,
    cancellation: CancellationToken,
) -> Result<u8, HarnessError> {
    let format = request.options.output_format.unwrap_or(if request.json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });
    // Resolve the provider before opening anything: an unconfigured environment
    // must fail fast with instructions and must not create state. The explicit
    // mock profile skips the provider configuration entirely, and the JSON
    // result labels it as a fixture.
    let config_overrides = super::config::with_catalog_model(
        ConfigOverrides {
            approval: request.options.approval.clone(),
            allowed_tools: request.options.allowed_tools.clone(),
            disallowed_tools: request.options.disallowed_tools.clone(),
            model: request.options.model.clone(),
            ..ConfigOverrides::default()
        },
        &context.paths.data_dir,
    );
    // prime-agent's `--thinking`: a level the run names must be one.
    let thinking_override = match request.options.thinking.as_deref() {
        Some(level) => Some(harness_providers::ThinkingLevel::parse(level).ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "--thinking {level:?} is not a level: off, minimal, low, medium, high, xhigh or max"
                ),
            )
        })?),
        None => None,
    };
    let mut resolved_config = super::config::resolve_layers(
        &context.paths.config_file,
        &context.project.root,
        environment,
        &config_overrides,
    )?;
    super::config::apply_catalog_context_window(&mut resolved_config, &context.paths.data_dir);
    let tool_policy = super::permissions::build_tool_policy(
        &resolved_config.approval,
        &resolved_config.allow_rules,
        &resolved_config.deny_rules,
        None,
    )?;
    let child_policy = tool_policy.clone();
    let provider_config = if request.options.mock {
        None
    } else {
        let config = resolve_provider_with_overrides(
            &context.paths.config_file,
            &context.project.root,
            environment,
            &context.paths.data_dir,
            &config_overrides,
        )
        .map_err(|message| HarnessError::new(ErrorCode::ServiceUnavailable, message))?;
        // A key the app saved is read by the resolver at call time. Prove that here,
        // before a store is opened or a turn is admitted, so a saved-but-unreadable
        // key fails with an actionable message instead of mid-turn.
        validate_credential_file(environment, &context.paths.data_dir)
            .map_err(|message| HarnessError::new(ErrorCode::SecretNotGranted, message))?;
        Some(config)
    };

    // The prompt's Git facts start a `git` process: run it while the store
    // opens and the workspace is observed, not after.
    let git_facts = {
        let root = context.project.root.clone();
        tokio::task::spawn_blocking(move || super::service::prompt_git_facts(&root))
    };
    // Name the directory that could not be opened: an operator has to know which
    // path failed, and the typed code must survive the extra context.
    let store_dir = context.project_store_dir();
    // A worker's run leases the store its agents share; the lease is held until
    // the run is done.
    // A run of its own opens the project's shared store, the one its delegated
    // children lease: each child opening a writer of its own failed with
    // `writer_locked` while the run held the lock.
    let (store, lease) = match &run_store {
        RunStore::Own => {
            let lease = super::store_lease::SharedStore::for_dir(store_dir.clone())
                .lease()
                .await
                .map_err(|error| {
                    HarnessError::new(
                        error.code(),
                        format!(
                            "cannot open the project store at {}: {error}",
                            store_dir.display()
                        ),
                    )
                })?;
            (lease.store(), lease)
        }
        RunStore::Shared(shared) => {
            let lease = shared.lease().await?;
            (lease.store(), lease)
        }
    };
    acceptance_trace("writer_opened");
    // Resuming continues the task of the named session: the new turn runs in a
    // fresh session linked to it, exactly like a follow-up in the interactive app.
    let resumed_from = match &request.resume {
        Some(selector) => Some(super::service::resolve_resume_session(&store, selector).await?),
        None => None,
    };
    let task_id = resumed_from
        .as_ref()
        .map_or_else(TaskId::generate, |(_, task)| task.clone());
    let session_id = SessionId::generate();
    // One project identity per workspace root, resolved before anything scoped to the
    // project is written.
    let project_id = project::resolve_project_id(&store, &context.project.root).await?;
    let observation = observe_workspace(project_id.clone(), &context.project.root)?;
    acceptance_trace("workspace_observed");
    let capabilities = match &provider_config {
        Some(config) => ModelCapabilities {
            provider_id: config.provider_id.clone(),
            model: config.model.clone(),
            supports_streaming: true,
            supports_tools: true,
            fixture: false,
        },
        None => ModelCapabilities {
            provider_id: "mock".to_owned(),
            model: "mock-profile".to_owned(),
            supports_streaming: true,
            supports_tools: false,
            fixture: true,
        },
    };
    let provider: Arc<dyn ModelProvider> = match &provider_config {
        Some(config) => super::service::build_provider(
            config,
            Arc::new(EnvironmentCredential::new(
                config.provider_id.clone(),
                config.credential_variable(),
                context.paths.data_dir.clone(),
            )),
            capabilities,
            thinking_override.unwrap_or_else(|| {
                harness_providers::ThinkingLevel::parse(&config.thinking).unwrap_or_default()
            }),
            task_id.as_ref(),
            &context.paths.data_dir,
        )
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
        None => Arc::new(MockProvider::text("mock profile: no model was called")),
    };
    let mut runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::clone(&provider),
        RuntimeConfig {
            context_window_tokens: resolved_config.context_window_tokens,
            output_reservation_tokens: resolved_config.output_reservation_tokens,
            compaction_reserve_tokens: resolved_config.compaction_reserve_tokens,
            compaction_ceiling_tokens: super::config::compaction_ceiling(environment),
            compaction_threshold_percent: super::config::compaction_threshold_percent(
                &context.paths.config_file,
            ),
            ..RuntimeConfig::default()
        },
    );
    // A token budget is an explicit account the run reserves against; without
    // one the run has no token bound to promise.
    if let Some(limit) = request.options.budget_tokens {
        let budget_id = BudgetId::generate();
        let ledger = BudgetLedger::new(Arc::clone(&store));
        ledger
            .ensure_account(&budget_id, None, limit)
            .await
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        runtime = runtime.with_budget(ledger, budget_id);
    }
    let runtime = Arc::new(runtime);
    // Local extensions are opt-in, loaded for this one turn and stopped afterwards.
    let extension_root = extensions::extensions_root(environment, &context.paths.data_dir);
    let active_extensions = if extensions::extensions_requested_from_environment(environment) {
        match extensions::load_active(&extension_root).await {
            Ok(active) => {
                out.stderr(&active.report().message(&extension_root));
                Some(active)
            }
            Err(error) => {
                out.stderr(&format!("extensions: not loaded ({error})"));
                None
            }
        }
    } else {
        None
    };
    // The web tools, as the app has them: a scripted run that is asked about
    // current events can search and read pages too. `HA_WEB=off` removes them.
    let web_host = super::web::WebHost::from_environment(
        environment,
        &context.paths.data_dir,
        resolved_config.web_search.as_deref(),
    );
    let hashline = super::config::HashlineRule::load(
        &context.paths.config_file,
        environment.value("HA_HASHLINE"),
    )
    .applies(&format!(
        "{}/{}",
        resolved_config.provider.id, resolved_config.provider.model
    ));
    let mut tools = ToolExecutionService::new(Arc::clone(&store))
        .with_hashline(hashline)
        .with_policy(tool_policy)
        .with_hooks(resolved_config.hooks.clone());
    // The jev plugin's harness decisions, when it is on and has a key.
    let jev_decisions = super::jev::JevDecisions::from_settings(
        environment,
        &context.paths.data_dir,
        &context.paths.config_file,
    );
    if let Some(decisions) = &jev_decisions {
        tools = tools.with_guard(Arc::clone(decisions) as Arc<dyn harness_tools::ToolGuard>);
    }
    let knowledge_host =
        super::knowledge::KnowledgeHost::new(&context.project.root, &context.paths.data_dir);
    // prime-agent 9eec445a: `ha exec` delegates as the app does, and the run
    // waits for its children. Nobody can answer an approval here, so a gated
    // action a child asks for is refused, as the root's is.
    let (child_events, mut child_inbox) = tokio::sync::mpsc::unbounded_channel::<SessionEvent>();
    let agents = provider_config.as_ref().and_then(|config| {
        let agents = super::delegation::SessionAgents::new(
            super::store_lease::SharedStore::for_dir(context.project_store_dir()),
            child_events,
            Arc::new(RefusingApprovals) as Arc<dyn harness_tools::ApprovalGate>,
            context.paths.data_dir.join("delegation"),
        )
        .ok()?;
        agents.bind_ledger(task_id.as_str());
        agents.set_max_workers(resolved_config.agents_max_workers.unwrap_or(0));
        agents.set_judge(
            jev_decisions
                .clone()
                .map(|decisions| decisions as Arc<dyn super::delegation::ChildJudge>),
        );
        agents.set_guard(
            jev_decisions
                .clone()
                .map(|decisions| decisions as Arc<dyn harness_tools::ToolGuard>),
        );
        agents.set_turn_judges(jev_decisions.clone().map(|decisions| {
            super::delegation::TurnJudges {
                visibility: Arc::clone(&decisions) as Arc<dyn harness_tools::ResultVisibility>,
                admission: Arc::clone(&decisions) as Arc<dyn harness_tools::OutputAdmission>,
                completion: decisions as Arc<dyn harness_tools::CompletionCheck>,
            }
        }));
        Some((agents, config))
    });
    let delegate_host = agents.as_ref().map(|(agents, config)| {
        let refinements = super::config::harness_refinements(&context.paths.config_file);
        super::delegation::DelegateHost::new(
            agents,
            super::delegation::ChildLaunch {
                model: super::delegation::ChildModel {
                    provider: Arc::clone(&provider),
                    reference: format!("{}/{}", config.provider_id, config.model),
                    price: config.model_price,
                    thinking: None,
                },
                models: None,
                runtime_config: RuntimeConfig {
                    context_window_tokens: resolved_config.context_window_tokens,
                    output_reservation_tokens: resolved_config.output_reservation_tokens,
                    compaction_reserve_tokens: resolved_config.compaction_reserve_tokens,
                    compaction_ceiling_tokens: super::config::compaction_ceiling(environment),
                    compaction_threshold_percent: super::config::compaction_threshold_percent(
                        &context.paths.config_file,
                    ),
                    ..RuntimeConfig::default()
                },
                workspace_root: context.project.root.clone(),
                workspace: observation.clone(),
                hooks: resolved_config.hooks.clone(),
                parent_policy: child_policy.clone(),
                child_limits: bounds::limits_from_environment(environment),
                hashline: super::config::HashlineRule::load(
                    &context.paths.config_file,
                    environment.value("HA_HASHLINE"),
                ),
                web: web_host
                    .as_ref()
                    .map(|host| (host.tools(), host.dispatcher())),
                checks: resolved_config.verify.checks.clone(),
                knowledge: Some(knowledge_host.readonly()),
                memory: Some(
                    super::memory::MemoryHost::new(&context.paths.data_dir, task_id.as_str())
                        .with_project(&context.project.root)
                        .with_refinements(refinements)
                        .readonly(),
                ),
                context: Vec::new(),
                // `ha exec`'s children get the root's memory too, ranked for their brief.
                digest: refinements.then(|| super::harness::DigestSource {
                    data_dir: context.paths.data_dir.clone(),
                    conversation: task_id.as_str().to_owned(),
                    project: Some(
                        super::bootstrap::project_key_of(&context.project.root)
                            .as_str()
                            .to_owned(),
                    ),
                }),
                follows: None,
            },
            cancellation.clone(),
        )
    });
    let dispatcher = super::mcp::combined_dispatcher_with_delegate(
        None,
        active_extensions.as_ref(),
        delegate_host.as_ref(),
        None,
        web_host.as_ref(),
        None,
        None,
        None,
    );
    let refinements = super::config::harness_refinements(&context.paths.config_file);
    let memory_host = super::memory::MemoryHost::new(&context.paths.data_dir, task_id.as_str())
        .with_project(&context.project.root)
        .with_refinements(refinements);
    if let Some((agents, _)) = &agents {
        tools = tools.with_elsewhere(Arc::clone(agents) as Arc<dyn harness_tools::PathElsewhere>);
    }
    // The jev plugin, when `/jev on` turned it on.
    let jev_host = super::jev::JevHost::from_settings(
        environment,
        &context.paths.data_dir,
        &context.paths.config_file,
    );
    tools = tools.with_external(super::jev::with_dispatcher(
        super::memory::with_dispatcher(
            super::knowledge::with_dispatcher(dispatcher, &knowledge_host),
            &memory_host,
        ),
        jev_host.as_ref(),
    ));
    let driver = TurnDriver::new(Arc::clone(&runtime), tools);
    let driver =
        match &jev_decisions {
            Some(decisions) => driver
                .with_visibility(Arc::clone(decisions) as Arc<dyn harness_tools::ResultVisibility>)
                .with_admission(Arc::clone(decisions) as Arc<dyn harness_tools::OutputAdmission>)
                .with_completion_check(
                    Arc::clone(decisions) as Arc<dyn harness_tools::CompletionCheck>
                ),
            None => driver,
        };
    let external_tools = super::mcp::combined_tools_with_delegate(
        None,
        active_extensions.as_ref(),
        delegate_host.as_ref(),
        None,
        web_host.as_ref(),
        None,
        None,
        None,
    );
    let external_tools = super::jev::with_tools(
        Some(super::memory::with_tools(
            Some(super::knowledge::with_tools(
                external_tools,
                &knowledge_host,
            )),
            &memory_host,
        )),
        jev_host.as_ref(),
    );
    let driver = match &external_tools {
        Some(external) => driver.with_external(external.clone()),
        None => driver,
    };
    // The goal, when the caller attached one, is host policy: typed criteria
    // and bounded continuations. An unknown criterion kind is a usage error,
    // not a silently ignored requirement.
    let goal_criteria = request
        .options
        .goal
        .as_ref()
        .map(|_| parse_criteria(&request.options.criteria))
        .transpose()?;
    let driver = match &request.options.goal {
        Some(objective) => {
            let criteria = goal_criteria.clone().unwrap_or_default();
            let spec = GoalSpec::new(objective.clone(), criteria);
            let no_progress = spec.max_no_progress;
            let goal = match request.options.max_continuations {
                Some(max) => spec.with_limits(max, max.min(no_progress)),
                None => spec,
            };
            driver.with_goal(goal)
        }
        None => driver,
    };
    // The durable steering/cancel inbox shares this turn's store.
    let driver = driver.with_inbox(RunInbox::new(Arc::clone(&store)));
    let mut tool_schemas = harness_tools::coding_tool_schemas_for(hashline);
    if let Some(external) = &external_tools {
        tool_schemas.extend(super::service::in_cache_order(external.schemas()));
    }
    // Content the prompt names rides with it, exactly as it does in the app: an image is
    // shown to the model, and a text file is put in the message. A scripted run can
    // therefore look at a screenshot or read a log the same way a person can.
    let attached = attachments::from_message(&request.prompt, &context.project.root);
    for note in &attached.notes {
        out.stderr(&format!("not attached ({note})"));
    }
    for notice in attachments::attachment_notices(&attached.images, &attached.files) {
        out.stderr(&notice);
    }
    let prompt = format!(
        "{}{}",
        request.prompt,
        attachments::attachment_blocks(&attached.files)
    );
    // The system prompt of the app, built from prime-agent's layers for the
    // tools this run has; a run used to get one generic sentence.
    // `--system-prompt` replaces the static layers and `--append-system-prompt`
    // adds to the end, as prime-agent's print mode takes them.
    let (system_prompt, environment_facts) = {
        let config_dir = context
            .paths
            .config_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        // prime-agent's resource loader: a `SYSTEM.md` stands in for
        // `--system-prompt`, an `APPEND_SYSTEM.md` for `--append-system-prompt`.
        let custom = request.options.system_prompt.clone().or_else(|| {
            super::resources::system_prompt(
                &context.project.root,
                config_dir,
                resolved_config.project_trusted,
            )
        });
        let appended = if request.options.append_system_prompt.is_empty() {
            super::resources::append_system_prompt(
                &context.project.root,
                config_dir,
                resolved_config.project_trusted,
            )
            .into_iter()
            .collect::<Vec<_>>()
        } else {
            request.options.append_system_prompt.clone()
        };
        let (git_branch, changed_files) = git_facts.await.unwrap_or_default();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let shell = if cfg!(windows) { "PowerShell" } else { "sh" };
        let tool_names = tool_schemas
            .iter()
            .filter_map(|schema| {
                schema
                    .pointer("/function/name")
                    .and_then(serde_json::Value::as_str)
            })
            .collect::<Vec<_>>();
        let prompt_environment = super::prompt::PromptEnvironment {
            os: std::env::consts::OS,
            shell,
            cwd: &context.project.root,
            project_root: &context.project.root,
            git_branch: git_branch.as_deref(),
            changed_files,
            date_iso: &today,
            limits: bounds::limits_from_environment(environment),
        };
        (
            super::prompt::assemble(
                custom.as_deref(),
                &prompt_environment,
                &tool_names,
                &appended,
            ),
            super::prompt::volatile_facts(&prompt_environment),
        )
    };
    let run_request = RunRequest::new(
        session_id.clone(),
        task_id.clone(),
        InputId::generate(),
        prompt,
        observation,
    )
    .with_tool_schemas(tool_schemas)
    .with_system_policy(system_prompt);
    // Memory is prime-agent's continual harness state, as in the app: this run reads
    // the digest of what the model has kept, ranked for this prompt.
    let harness_state = super::harness::merge(
        super::harness::for_project(
            super::harness::load(
                &super::harness::global_dir(&context.paths.data_dir),
                "global",
            ),
            Some(super::bootstrap::project_key_of(&context.project.root).as_str()),
        ),
        super::harness::load(
            &super::harness::local_dir(&context.paths.data_dir, task_id.as_str()),
            "local",
        ),
    );
    let harness_entries = harness_state.total();
    // The date and the Git facts ride the input, as in the app, so the system
    // prompt is the same from one run to the next and stays cached.
    let mut project_blocks = vec![harness_session::ContextBlock::mandatory(
        "environment",
        harness_session::ContextBlockKind::Instruction,
        environment_facts,
    )];
    // With harness refinements off, no saved entry reaches the model.
    if refinements {
        project_blocks.push(super::harness::digest_block(
            &super::harness::format_digest(
                &harness_state,
                super::harness::DigestOptions {
                    memory_tool: true,
                    ..super::harness::DigestOptions::default()
                },
                &super::harness::weighted_terms(None, &[request.prompt.as_str()]),
            ),
        ));
    }
    if let Some(block) = super::knowledge::context_block(&knowledge_host, &[(&request.prompt, 1.0)])
    {
        project_blocks.push(block);
    }
    let run_request = run_request.with_project_rules(project_blocks);
    let run_request = if attached.is_empty() {
        run_request
    } else {
        run_request.with_images(
            attached
                .images
                .into_iter()
                .map(|image| image.attachment)
                .collect(),
        )
    };
    let run_request_images = run_request
        .images
        .iter()
        .map(|image| image.label.clone())
        .collect::<Vec<_>>();
    // One object per attached file: a script needs the path it was read from as well as
    // the label a reader sees, and the byte count is what the turn's budget was spent on.
    let run_request_files: Vec<serde_json::Value> = attached
        .files
        .iter()
        .map(|file| {
            serde_json::json!({
                "path": file.path,
                "label": file.label,
                "bytes": file.bytes,
            })
        })
        .collect();
    let options = TurnOptions {
        workspace_root: context.project.root.clone(),
        actor_id: "headless.user".to_owned(),
        // There is nobody to ask: a gated action fails closed.
        approvals: headless_approval_mode(),
        // One turn, bounded as the environment asks: a script reads `stop` in the JSON
        // and resumes with `--resume` when it wants more, so the app never continues
        // silently in the middle of somebody's pipeline.
        limits: bounds::limits_from_environment(environment),
    };
    let observer = Arc::new(HeadlessObserver {
        auto_allowed: Mutex::new(Vec::new()),
        notices: Mutex::new(Vec::new()),
        stream_json: format == OutputFormat::StreamJson,
        approval_blocked: AtomicBool::new(false),
        out: Arc::clone(&out),
        tokens: AtomicU64::new(0),
        task: task_id.as_str().to_owned(),
    });
    observer.stream_event(
        "turn.started",
        &serde_json::json!({"session_id": session_id, "task_id": task_id}),
    );
    harness_types::diag::info("turn", "start")
        .task(task_id.as_str())
        .session(session_id.as_str())
        .field("headless", true)
        .emit();
    let turn_observer: Arc<dyn TurnObserver> = observer.clone();
    // prime-agent's headless autonomous run (`--autonomous*`): after a turn
    // that ends cleanly the gates run in the workspace, and the run goes on
    // with prime's continuation until they pass or a budget runs out.
    let mut autonomous = super::autonomous::Autonomous::default();
    if let Some(autonomous_options) = &request.options.autonomous {
        autonomous.turn_on(autonomous_options, std::time::Instant::now());
    }
    let mut continuing_from = resumed_from.as_ref().map(|(source, _)| source.clone());
    let mut next_request = run_request;
    let mut child_turns = 0;
    // What the run did, toward the conversation's automatic review.
    let mut turns_run = 0_u32;
    let mut tool_calls_run = 0_u32;
    let outcome = loop {
        let tokens_before = observer.tokens.load(Ordering::SeqCst);
        let outcome = match &continuing_from {
            Some(source) => {
                driver
                    .run_turn_continuing(
                        source,
                        next_request.clone(),
                        options.clone(),
                        Arc::clone(&turn_observer),
                        cancellation.clone(),
                    )
                    .await?
            }
            None => {
                driver
                    .run_turn(
                        next_request.clone(),
                        options.clone(),
                        Arc::clone(&turn_observer),
                        cancellation.clone(),
                    )
                    .await?
            }
        };
        turns_run = turns_run.saturating_add(1);
        tool_calls_run = tool_calls_run.saturating_add(outcome.tool_calls);
        // Children started with `wait: false` report when they settle; their
        // notices open one more turn, as they do in the app.
        if let Some((agents, _)) = &agents
            && outcome.stop == harness_tools::TurnStop::Final
            && child_turns < CHILD_TURNS
        {
            let notices = settle_children(agents, &mut child_inbox, &cancellation).await;
            if !notices.is_empty() {
                child_turns += 1;
                harness_types::diag::info("turn", "children_settled")
                    .task(next_request.task_id.as_str())
                    .session(outcome.session_id.as_str())
                    .field("notices", notices.len())
                    .field("turn", child_turns)
                    .emit();
                observer.stream_event(
                    "children.settled",
                    &serde_json::json!({"notices": notices.len()}),
                );
                continuing_from = Some(outcome.session_id.clone());
                // The finished turn still holds the task; the next one takes it
                // over, as each turn of the app does.
                release_task(&store, &next_request.task_id, &observer).await;
                next_request = RunRequest::new(
                    SessionId::generate(),
                    next_request.task_id.clone(),
                    InputId::generate(),
                    notices.join("\n\n"),
                    observe_workspace(project_id.clone(), &context.project.root)?,
                )
                .with_tool_schemas(next_request.tool_schemas.clone())
                .with_system_policy(next_request.system_policy.clone())
                .with_project_rules(next_request.project_rules.clone());
                continue;
            }
        }
        if !autonomous.enabled {
            break outcome;
        }
        autonomous.record_turn(
            observer
                .tokens
                .load(Ordering::SeqCst)
                .saturating_sub(tokens_before),
        );
        let clean =
            outcome.stop == harness_tools::TurnStop::Final && outcome.pending_question.is_none();
        let gates = match autonomous.gate_job() {
            Some(job) if clean => Some(
                super::autonomous::run_gates(&context.project.root, job, cancellation.clone())
                    .await,
            ),
            _ => None,
        };
        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        match autonomous.decide(clean, gates, std::time::Instant::now(), &timestamp) {
            super::autonomous::Decision::Continue(text) => {
                observer.stream_event(
                    "autonomous.continuation",
                    &serde_json::json!({
                        "continuations": autonomous.continuations_used,
                        "text": text,
                    }),
                );
                continuing_from = Some(outcome.session_id.clone());
                release_task(&store, &next_request.task_id, &observer).await;
                next_request = RunRequest::new(
                    SessionId::generate(),
                    next_request.task_id.clone(),
                    InputId::generate(),
                    text,
                    observe_workspace(project_id.clone(), &context.project.root)?,
                )
                .with_tool_schemas(next_request.tool_schemas.clone())
                .with_system_policy(next_request.system_policy.clone())
                .with_project_rules(next_request.project_rules.clone());
            }
            super::autonomous::Decision::Stop(_) => break outcome,
        }
    };
    if let Some(question_id) = &outcome.pending_question {
        observer.stream_event("ask_user", &serde_json::json!({"question_id": question_id}));
    }
    let acceptance_command_id = if outcome.acceptance == harness_runtime::AcceptanceState::Satisfied
    {
        let criteria = accepted_criteria(goal_criteria.as_deref().unwrap_or_default(), &outcome)?;
        let command = AcceptanceCommand::Evaluate {
            criteria: criteria.clone(),
            pending_effects: 0,
            evidence_fingerprint: outcome.executions.iter().rev().find_map(|view| {
                view.receipt
                    .as_ref()
                    .and_then(|receipt| receipt.after_fingerprint.clone())
            }),
        };
        let transition = AcceptanceRecord::initial(outcome.task_id.clone())
            .apply(command)
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        let command_id = RuntimeCommandId::generate();
        store
            .record_acceptance_command(
                &command_id,
                &outcome.run_id,
                &transition.next,
                &serde_json::json!({
                    "kind": "evaluate",
                    "criteria": criteria,
                    "pending_effects": 0,
                    "evidence_fingerprint": transition.next.evidence_fingerprint,
                }),
            )
            .await
            .map_err(StoreError::into_harness_error)?;
        Some(command_id)
    } else {
        None
    };
    let auto_allowed = observer
        .auto_allowed
        .lock()
        .map_or_else(|_| Vec::new(), |messages| messages.clone());
    let mut notices = observer
        .notices
        .lock()
        .map_or_else(|_| Vec::new(), |messages| messages.clone());
    if let Some(notice) = &resolved_config.context_window_notice {
        notices.push(notice.clone());
    }
    acceptance_trace("turn_finished");
    let memory_report = serde_json::json!({
        "kind": "harness",
        "entries": harness_entries,
    });
    let extensions_report = match &active_extensions {
        Some(active) => serde_json::json!({
            "enabled": true,
            "plugins": active.report().plugins,
            "tools": active.report().tools,
            "refused": active.report().refused,
        }),
        None => serde_json::json!({"enabled": false}),
    };
    // Run status and task acceptance are separate fields: a run that stopped
    // cleanly is not the same claim as a task that was accepted.
    let run_report = serde_json::json!({
        "run_id": outcome.run_id,
        "state": run_state_label(&store, &outcome).await,
        "stop": outcome.stop.as_str(),
        "stop_reason": outcome.stop.as_str(),
        "steps": outcome.steps,
        "tool_calls": outcome.tool_calls,
    });
    let goal_report = match &outcome.goal {
        Some(goal) => serde_json::json!({
            "objective": goal.objective,
            "acceptance": goal.acceptance.as_str(),
            "verdict": goal.verdict,
            "continuations": goal.continuations,
            "no_progress": goal.no_progress,
            "stop": goal.stop.map(harness_tools::TurnStop::as_str),
            "missing": goal.missing,
        }),
        None => serde_json::Value::Null,
    };
    let pending_question = match &outcome.pending_question {
        Some(question_id) => {
            let service = HumanInputService::new(Arc::clone(&store));
            let question = service.question(question_id).await.ok().flatten();
            serde_json::json!({
                "question_id": question_id,
                "state": question.as_ref().map(|question| question.state.as_str()),
                "prompt": question.as_ref().map(|question| question.prompt.clone()),
                "scope_key": question.as_ref().map(|question| question.scope_key.clone()),
            })
        }
        None => serde_json::Value::Null,
    };
    harness_types::diag::at(
        if outcome.stop.as_str() == "final" {
            harness_types::diag::Level::Info
        } else {
            harness_types::diag::Level::Warn
        },
        "turn",
        "end",
    )
    .task(outcome.task_id.as_str())
    .session(outcome.session_id.as_str())
    .field("stop", outcome.stop.as_str())
    .field("steps", outcome.steps)
    .field("tool_calls", outcome.tool_calls)
    .field("headless", true)
    .emit();
    let mut output = serde_json::json!({
        "schema_version": 1,
        "session_id": outcome.session_id,
        "task_id": outcome.task_id,
        "input_id": outcome.input_id,
        "response": outcome.final_text,
        "steps": outcome.steps,
        "tool_calls": outcome.tool_calls,
        "stop": outcome.stop.as_str(),
        "run": run_report,
        "acceptance": outcome.acceptance.as_str(),
        "acceptance_command_id": acceptance_command_id,
        "goal": goal_report,
        "pending_question": pending_question,
        "approvals": "none",
        "auto_allowed": auto_allowed,
        "notices": notices,
        "fixture": request.options.mock,
        "memory": memory_report,
        "extensions": extensions_report,
        "images": run_request_images,
        "files": run_request_files,
        "resumed_from": resumed_from
            .as_ref()
            .map(|(source, _)| source.as_str().to_owned()),
    });
    if request.options.output_format.is_some() {
        output["acceptance"] = serde_json::json!({
            "state": outcome.acceptance.as_str(),
            "command_id": acceptance_command_id,
        });
    }

    // The automatic review, as the app runs one at a session's end: what the
    // conversation's runs did since its last review is counted, and when it is
    // enough the review reads the conversation while the store is still open.
    // A run that answered nothing (a mock, a cancel) is not reviewed.
    let review = if provider_config.is_some()
        && refinements
        && super::service::auto_refine_enabled(&context.paths.config_file, environment)
        && !cancellation.is_cancelled()
        && outcome.stop != harness_tools::TurnStop::Canceled
    {
        let local = super::harness::local_dir(&context.paths.data_dir, outcome.task_id.as_str());
        match super::refine::exec_review_due(
            &local,
            turns_run,
            tool_calls_run,
            runtime.compactions() > 0,
            super::service::auto_refine_interval(&context.paths.config_file),
        ) {
            Some(turns) => {
                let conversation =
                    super::service::refine_transcript(&store, &outcome.session_id, &local).await;
                (!conversation.is_empty()).then_some((turns, conversation, local))
            }
            None => None,
        }
    } else {
        None
    };

    drop(driver);
    // Stop the extension processes this turn started, before the writer is released.
    if let Some(active) = active_extensions {
        active.shutdown().await;
    }
    drop(runtime);
    drop(store);
    // A run of its own closes the store before it returns; a worker's agents
    // share theirs and keep it open for the next turn.
    if matches!(run_store, RunStore::Own) {
        lease.end().await?;
    } else {
        drop(lease);
    }
    acceptance_trace("writer_closed");

    if format == OutputFormat::Json {
        out.stdout(&output.to_string());
    } else if format == OutputFormat::StreamJson {
        observer.stream_event("run.terminal", &output);
    } else {
        for message in notices {
            out.stdout(&format!("[info] {message}"));
        }
        for message in auto_allowed {
            out.stdout(&format!("[info] {message}"));
        }
        out.stdout(output["response"].as_str().unwrap_or_default());
    }
    // The answer is out; the review runs before the process exits, since no
    // process is left to run it beside the conversation. What it says goes to
    // stderr, and Ctrl-C cuts it off (whatever it wrote is undone).
    if let Some((turns, conversation, local)) = review {
        out.stderr(&format!(
            "auto-refine: reviewing the last {turns} turn(s) of this conversation before exiting (HA_AUTO_REFINE=off skips it)"
        ));
        let config_dir = context
            .paths
            .config_file
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let learning = super::service::refine_learning(
            config_dir,
            &context.paths.data_dir,
            &context.project.root,
            environment,
            resolved_config.project_trusted,
        );
        let scopes = super::refine::HarnessScopes {
            global: super::harness::global_dir(&context.paths.data_dir),
            local,
        };
        let model = provider_config
            .as_ref()
            .map(|config| config.model.clone())
            .unwrap_or_default();
        let reviewed = async {
            let mut events = super::service::review_now(
                Arc::clone(&provider),
                model.clone(),
                conversation,
                outcome.task_id.as_str().to_owned(),
                scopes.clone(),
                learning,
                turns,
                Some(outcome.session_id.as_str().to_owned()),
                knowledge_host.clone(),
            )
            .await;
            // A conversation's store that outgrew forty entries is
            // consolidated too, as the app does after a turn.
            if super::refine::local_consolidation_due(&scopes.local)
                && let Some(_consolidating) = super::refine::Consolidating::try_begin()
            {
                events.push(
                    super::service::consolidate_local_event(
                        &provider,
                        &model,
                        &scopes,
                        outcome.task_id.as_str(),
                    )
                    .await,
                );
            }
            events
        };
        tokio::select! {
            events = reviewed => {
                for event in events {
                    report_review(out.as_ref(), &event);
                }
            }
            () = cancellation.cancelled() => {
                out.stderr("auto-refine: the review was cut off");
            }
        }
    }
    // prime-agent's print-mode autonomous contract applies to every output.
    if let Some(line) = autonomous.exit_stderr(std::time::Instant::now()) {
        out.stderr(&line);
        return Ok(1);
    }
    let exit = match outcome.stop {
        _ if request.options.output_format.is_none() => 0,
        harness_tools::TurnStop::Canceled => 130,
        harness_tools::TurnStop::NeedsInput | harness_tools::TurnStop::ExternalWait => 3,
        _ if observer.approval_blocked.load(Ordering::SeqCst) => 3,
        harness_tools::TurnStop::Final
            if outcome.goal.as_ref().is_none_or(|goal| {
                goal.acceptance == harness_runtime::AcceptanceState::Satisfied
            }) =>
        {
            0
        }
        _ => 4,
    };
    Ok(exit)
}

/// Say on stderr what the automatic review did.
fn report_review(out: &dyn Output, event: &SessionEvent) {
    match event {
        SessionEvent::Notice { message } => out.stderr(message),
        SessionEvent::Refined {
            header,
            summary,
            details,
        } => {
            out.stderr(&format!("{header}: {summary}"));
            for line in details {
                out.stderr(&format!("  {line}"));
            }
        }
        _ => {}
    }
}

/// Convert the goal evaluator's proven evidence into M0's durable command input.
fn accepted_criteria(
    criteria: &[GoalCriterion],
    outcome: &TurnOutcome,
) -> Result<Vec<CriterionState>, HarnessError> {
    criteria
        .iter()
        .map(|criterion| {
            let (source, bytes) = if criterion.evidence == EvidenceKind::Response {
                (
                    format!("run:{}:response", outcome.run_id),
                    outcome.final_text.as_bytes().to_vec(),
                )
            } else {
                let final_digest = outcome.executions.iter().rev().find_map(|view| {
                    view.receipt
                        .as_ref()
                        .and_then(|receipt| receipt.after_fingerprint.as_ref())
                });
                let view = outcome
                    .executions
                    .iter()
                    .find(|view| {
                        let receipt = view.receipt.as_ref();
                        match criterion.evidence {
                            EvidenceKind::Response => false,
                            EvidenceKind::ToolExecution => receipt.is_some_and(|receipt| {
                                receipt.outcome_state == ToolOutcomeState::Settled
                            }),
                            EvidenceKind::FileChange => matches!(
                                view.output,
                                ToolOutput::ApplyPatch { .. }
                                    | ToolOutput::WriteFile { .. }
                                    | ToolOutput::EditFile { .. }
                            ),
                            EvidenceKind::Check => {
                                matches!(
                                    view.output,
                                    ToolOutput::Process {
                                        exit_code: Some(0),
                                        timed_out: false,
                                        canceled: false,
                                        ..
                                    }
                                ) && receipt.and_then(|receipt| receipt.after_fingerprint.as_ref())
                                    == final_digest
                            }
                            EvidenceKind::Artifact => {
                                receipt.is_some_and(|receipt| receipt.artifact_id.is_some())
                            }
                        }
                    })
                    .ok_or_else(|| {
                        HarnessError::new(
                            ErrorCode::InvalidStateTransition,
                            format!(
                                "satisfied goal lacks committed {} evidence",
                                criterion.evidence.as_str()
                            ),
                        )
                    })?;
                let receipt = view.receipt.as_ref().ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidStateTransition,
                        "accepted tool evidence has no receipt",
                    )
                })?;
                let bytes = serde_json::to_vec(receipt).map_err(|error| {
                    HarnessError::new(
                        ErrorCode::StorageWriteFailed,
                        format!("serialize evidence receipt: {error}"),
                    )
                })?;
                (format!("tool_receipt:{}", receipt.tool_execution_id), bytes)
            };
            Ok(CriterionState {
                criterion_id: criterion.id.clone(),
                required: criterion.required,
                status: CriterionStatus::Satisfied,
                evidence: vec![CriterionEvidence::RunObserved {
                    run_id: outcome.run_id.clone(),
                    evidence_kind: criterion.evidence.as_str().to_owned(),
                    source,
                    content_hash: ContentHash::from_bytes(&bytes),
                }],
            })
        })
        .collect()
}

/// An absent user cannot answer a gate, so every `Ask` decision must refuse.
/// The most turns the children's reports may open after the first.
const CHILD_TURNS: u32 = 8;

/// Let the next turn of a run take its task over from the turn that finished.
///
/// A finished turn's session keeps the task's lease in the open store, and a
/// new session of the same host could not claim it: `ha exec`'s turn for its
/// children's reports failed with `task_lease_conflict`.
async fn release_task(store: &SqliteStore, task_id: &TaskId, observer: &HeadlessObserver) {
    if let Err(error) = store.release_task_lease(task_id).await {
        observer.stream_event(
            "notice",
            &serde_json::json!({
                "message": format!("the previous turn's hold on the task was kept: {error}")
            }),
        );
    }
}

/// The longest a run waits for its children to settle.
const CHILD_WAIT: std::time::Duration = std::time::Duration::from_mins(30);

/// Wait until no child of the run is working, and return the notices of those
/// that settled with nobody waiting for them (prime-agent's quiescence wait).
async fn settle_children(
    agents: &Arc<super::delegation::SessionAgents>,
    inbox: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    cancellation: &CancellationToken,
) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + CHILD_WAIT;
    let mut notices = Vec::new();
    loop {
        while let Ok(event) = inbox.try_recv() {
            notices.extend(child_report(event));
        }
        if agents.running() == 0 || cancellation.is_cancelled() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = agents.stop("all", "the run stopped waiting for it");
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            () = cancellation.cancelled() => {}
        }
    }
    while let Ok(event) = inbox.try_recv() {
        notices.extend(child_report(event));
    }
    notices
}

/// What a child told the run: its terminal notice, or the message it sent
/// its parent. A child that answered by message settles with no notice (its
/// answer was the message), and `ha exec` dropped that message: asked for two
/// counts, it reported the one child that answered by notice and ended.
fn child_report(event: SessionEvent) -> Option<String> {
    match event {
        SessionEvent::ChildSettled { notice, .. } => Some(notice),
        SessionEvent::AgentMessage { text } => Some(text),
        _ => None,
    }
}

/// A run with nobody to ask refuses what needs approval.
struct RefusingApprovals;

impl harness_tools::ApprovalGate for RefusingApprovals {
    fn request(
        &self,
        _proposal: harness_tools::ApprovalProposal,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = harness_tools::ApprovalAnswer> + Send>>
    {
        Box::pin(async { harness_tools::ApprovalAnswer::Denied })
    }
}

pub(super) fn headless_approval_mode() -> ApprovalMode {
    ApprovalMode::None
}

#[cfg(test)]
mod tests {
    use super::child_report;
    use crate::interactive::events::SessionEvent;

    /// Seen in prime.sh's `ha exec` check: asked to count the .txt files of
    /// two folders, one explorer answered with its notice and the other by
    /// message to its parent; the run reported only the first and ended.
    #[test]
    fn a_childs_message_is_a_report_as_its_notice_is() {
        assert_eq!(
            child_report(SessionEvent::ChildSettled {
                name: "explorer-b".to_owned(),
                notice: "[child-exited: no-reply child:explorer-b]\n\nLast assistant text: 2"
                    .to_owned(),
            }),
            Some("[child-exited: no-reply child:explorer-b]\n\nLast assistant text: 2".to_owned())
        );
        assert_eq!(
            child_report(SessionEvent::AgentMessage {
                text: "[agent-message from child:explorer-a]\n3".to_owned(),
            }),
            Some("[agent-message from child:explorer-a]\n3".to_owned())
        );
        assert_eq!(
            child_report(SessionEvent::Notice {
                message: "[child explorer-a] read_file: path=a".to_owned(),
            }),
            None,
            "progress is not a report"
        );
    }
}
