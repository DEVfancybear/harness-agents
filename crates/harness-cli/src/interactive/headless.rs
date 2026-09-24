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
    atomic::{AtomicBool, Ordering},
};

use harness_providers::{
    CancellationToken, DeepSeekAdapter, MockProvider, ModelCapabilities, ModelProvider,
};
use harness_runtime::{
    BudgetLedger, EvidenceKind, GoalCriterion, GoalSpec, HumanInputService, RunInbox, RunRequest,
    RuntimeConfig, RuntimeService,
};
use harness_store_sqlite::{SqliteStore, StoreError, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, ToolExecutionService, ToolOutput, TurnDriver, TurnObserver, TurnOptions,
    TurnOutcome, TurnProgress, coding_tool_schemas, observe_workspace,
};
use harness_types::{
    AcceptanceCommand, AcceptanceRecord, BudgetId, ContentHash, CriterionEvidence, CriterionState,
    CriterionStatus, ErrorCode, HarnessError, HostId, InputId, RuntimeCommandId, SessionId, TaskId,
    ToolOutcomeState,
};

use super::HeadlessOptions;
use super::attachments;
use super::bootstrap::{self, LaunchRequest};
use super::bounds;
use super::config::ConfigOverrides;
use super::extensions;
use super::memory;
use super::paths::{HostPlatform, LaunchEnvironment};
use super::project;
use super::service::{
    EnvironmentCredential, resolve_provider_with_overrides, validate_credential_file,
};

/// A validated single-turn headless request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessRequest {
    pub prompt: String,
    pub json: bool,
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
    pub options: HeadlessOptions,
}

/// Machine output stays line-oriented and contains no terminal control codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

/// Auto-allowed actions remain visible in headless JSON and plain output.
#[derive(Default)]
struct HeadlessObserver {
    auto_allowed: Mutex<Vec<String>>,
    notices: Mutex<Vec<String>>,
    stream_json: bool,
    approval_blocked: AtomicBool,
}

impl HeadlessObserver {
    fn stream_event(&self, kind: &str, fields: &serde_json::Value) {
        if self.stream_json {
            println!("{}", serde_json::json!({"type": kind, "data": fields}));
        }
    }
}

impl TurnObserver for HeadlessObserver {
    fn observe(&self, progress: TurnProgress) {
        match &progress {
            TurnProgress::TextDelta(text) => self.stream_event("text.delta", &serde_json::json!({"text": text})),
            TurnProgress::ThinkingDelta(text) => self.stream_event("thinking.delta", &serde_json::json!({"text": text})),
            TurnProgress::Usage { prompt_tokens, completion_tokens } => self.stream_event("usage", &serde_json::json!({"prompt_tokens": prompt_tokens, "completion_tokens": completion_tokens})),
            TurnProgress::ToolStarted { name, summary } => self.stream_event("tool.started", &serde_json::json!({"name": name, "summary": summary})),
            TurnProgress::ToolSettled { name, ok, detail } => {
                self.stream_event("tool.settled", &serde_json::json!({"name": name, "ok": ok, "detail": detail}));
                if !ok && detail.as_deref().is_some_and(|text| text.contains("approval") || text.contains("denied by the user")) {
                    self.approval_blocked.store(true, Ordering::SeqCst);
                    self.stream_event("approval.blocked", &serde_json::json!({"name": name, "reason": detail}));
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
/// The asset id one memory write produced, when it produced one.
fn memory_asset_of(
    remembered: &Result<Option<memory::RememberOutcome>, HarnessError>,
) -> Option<String> {
    match remembered {
        Ok(Some(
            memory::RememberOutcome::Stored(asset_id)
            | memory::RememberOutcome::Duplicate(asset_id)
            | memory::RememberOutcome::StoredButUnpruned { asset_id, .. },
        )) => Some(asset_id.as_str().to_owned()),
        _ => None,
    }
}

/// What one memory write did, in words a scripting caller can branch on.
fn memory_disposition_of(
    remembered: &Result<Option<memory::RememberOutcome>, HarnessError>,
) -> &'static str {
    match remembered {
        Ok(Some(memory::RememberOutcome::Stored(_))) => "stored",
        // Distinct from `stored` because the caller's next question is about the store, not
        // about this turn: a log that could not be trimmed keeps growing.
        Ok(Some(memory::RememberOutcome::StoredButUnpruned { .. })) => "stored_but_unpruned",
        Ok(Some(memory::RememberOutcome::Duplicate(_))) => "duplicate",
        Ok(Some(memory::RememberOutcome::NotKnowledge { reason })) => reason,
        Ok(Some(memory::RememberOutcome::NothingAdmitted) | None) => "nothing_admitted",
        Err(_) => "error",
    }
}

/// Run one bounded headless turn and report it as JSON.
///
/// The body is a linear sequence: resolve, run exactly one turn, shut down. It is
/// long because it wires real components, not because it branches.
#[allow(clippy::too_many_lines)]
pub async fn run(request: HeadlessRequest) -> Result<ExitCode, HarnessError> {
    let format = request.options.output_format.unwrap_or(if request.json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    });
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
        caller_dir,
        platform: HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    })?;
    acceptance_trace("bootstrap_resolved");
    // Resolve the provider before opening anything: an unconfigured environment
    // must fail fast with instructions and must not create state. The explicit
    // mock profile skips the provider configuration entirely, and the JSON
    // result labels it as a fixture.
    let config_overrides = ConfigOverrides {
        approval: request.options.approval.clone(),
        allowed_tools: request.options.allowed_tools.clone(),
        disallowed_tools: request.options.disallowed_tools.clone(),
        ..ConfigOverrides::default()
    };
    let resolved_config = super::config::resolve_layers(
        &context.paths.config_file,
        &context.project.root,
        &environment,
        &config_overrides,
    )?;
    let tool_policy = super::permissions::build_tool_policy(
        &resolved_config.approval,
        &resolved_config.allow_rules,
        &resolved_config.deny_rules,
        None,
    )?;
    let provider_config = if request.options.mock {
        None
    } else {
        let config = resolve_provider_with_overrides(
            &context.paths.config_file,
            &context.project.root,
            &environment,
            &context.paths.data_dir,
            &config_overrides,
        )
        .map_err(|message| HarnessError::new(ErrorCode::ServiceUnavailable, message))?;
        // A key the app saved is read by the resolver at call time. Prove that here,
        // before a store is opened or a turn is admitted, so a saved-but-unreadable
        // key fails with an actionable message instead of mid-turn.
        validate_credential_file(&environment, &context.paths.data_dir)
            .map_err(|message| HarnessError::new(ErrorCode::SecretNotGranted, message))?;
        Some(config)
    };

    // Name the directory that could not be opened: an operator has to know which
    // path failed, and the typed code must survive the extra context.
    let store_dir = context.project_store_dir();
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            store_dir.clone(),
            HostId::generate(),
        ))
        .await
        .map_err(|error| {
            HarnessError::new(
                error.code(),
                format!(
                    "cannot open the project store at {}: {error}",
                    store_dir.display()
                ),
            )
        })?,
    );
    acceptance_trace("writer_opened");
    // Resuming continues the task of the named session: the new turn runs in a
    // fresh session linked to it, exactly like a follow-up in the interactive app.
    let resumed_from = match &request.resume {
        Some(session_text) => {
            let session_text = if session_text == "latest" {
                store
                    .newest_session_id()
                    .await
                    .map_err(StoreError::into_harness_error)?
                    .map(|id| id.as_str().to_owned())
                    .ok_or_else(|| {
                        HarnessError::new(
                            ErrorCode::InvalidPayload,
                            "this project has no session to continue",
                        )
                    })?
            } else {
                session_text.clone()
            };
            let parsed = SessionId::parse(session_text.clone()).map_err(|error| {
                HarnessError::new(
                    error.code(),
                    format!("--resume needs a canonical session id: {error}"),
                )
            })?;
            let task = store
                .session_task(&parsed)
                .await
                .map_err(StoreError::into_harness_error)?
                .ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        format!(
                            "session {session_text} is not in this project's store; nothing was resumed"
                        ),
                    )
                })?;
            Some((parsed, task))
        }
        None => None,
    };
    let task_id = resumed_from
        .as_ref()
        .map_or_else(TaskId::generate, |(_, task)| task.clone());
    let session_id = SessionId::generate();
    // One project identity per workspace root, resolved before anything scoped to the
    // project is written; memory stays a separate opt-in on top of it.
    let project_id = project::resolve_project_id(&store, &context.project.root).await?;
    let memory_principal = if memory::memory_requested_from_environment(&environment) {
        Some(memory::principal(
            project_id.clone(),
            task_id.clone(),
            session_id.clone(),
        ))
    } else {
        None
    };
    let observation = observe_workspace(project_id, &context.project.root)?;
    acceptance_trace("workspace_observed");
    let capabilities = match &provider_config {
        Some(config) => ModelCapabilities {
            provider_id: "deepseek".to_owned(),
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
        Some(config) => Arc::new(
            DeepSeekAdapter::new(
                config.endpoint.clone(),
                Arc::new(EnvironmentCredential::new(
                    config.credential_variable(),
                    context.paths.data_dir.clone(),
                )),
                capabilities,
            )
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
        ),
        None => Arc::new(MockProvider::text("mock profile: no model was called")),
    };
    let mut runtime = RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig {
            context_window_tokens: resolved_config.context_window_tokens,
            output_reservation_tokens: resolved_config.output_reservation_tokens,
            compaction_reserve_tokens: resolved_config.compaction_reserve_tokens,
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
    let extension_root = extensions::extensions_root(&environment, &context.paths.data_dir);
    let active_extensions = if extensions::extensions_requested_from_environment(&environment) {
        match extensions::load_active(&extension_root).await {
            Ok(active) => {
                eprintln!("{}", active.report().message(&extension_root));
                Some(active)
            }
            Err(error) => {
                eprintln!("extensions: not loaded ({error})");
                None
            }
        }
    } else {
        None
    };
    let tools = match &active_extensions {
        Some(active) => ToolExecutionService::new(Arc::clone(&store))
            .with_policy(tool_policy)
            .with_hooks(resolved_config.hooks.clone())
            .with_external(active.dispatcher()),
        None => ToolExecutionService::new(Arc::clone(&store))
            .with_policy(tool_policy)
            .with_hooks(resolved_config.hooks.clone()),
    };
    let driver = TurnDriver::new(Arc::clone(&runtime), tools);
    let driver = match &active_extensions {
        Some(active) => driver.with_external(active.tools()),
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
    let tool_schemas = match &active_extensions {
        Some(active) => {
            let mut schemas = coding_tool_schemas();
            schemas.extend(active.tools().schemas());
            schemas
        }
        None => coding_tool_schemas(),
    };
    // Content the prompt names rides with it, exactly as it does in the app: an image is
    // shown to the model, and a text file is put in the message. A scripted run can
    // therefore look at a screenshot or read a log the same way a person can.
    let attached = attachments::from_message(&request.prompt, &context.project.root);
    for note in &attached.notes {
        eprintln!("not attached ({note})");
    }
    for notice in attachments::attachment_notices(&attached.images, &attached.files) {
        eprintln!("{notice}");
    }
    let prompt = format!(
        "{}{}",
        request.prompt,
        attachments::attachment_blocks(&attached.files)
    );
    let run_request = RunRequest::new(
        session_id.clone(),
        task_id.clone(),
        InputId::generate(),
        prompt,
        observation,
    )
    .with_tool_schemas(tool_schemas);
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
    let mut recall = None;
    let run_request = match &memory_principal {
        Some(principal) => {
            match memory::recall(
                Arc::clone(&store),
                principal,
                &context.project.root,
                &request.prompt,
            )
            .await
            {
                Ok(found) => {
                    let request = run_request.with_memory(found.contribution.clone());
                    recall = Some(found);
                    request
                }
                Err(error) => {
                    eprintln!("memory: recall skipped ({error})");
                    run_request
                }
            }
        }
        None => run_request,
    };
    let options = TurnOptions {
        workspace_root: context.project.root.clone(),
        actor_id: "headless.user".to_owned(),
        // There is nobody to ask: a gated action fails closed.
        approvals: headless_approval_mode(),
        // One turn, bounded as the environment asks: a script reads `stop` in the JSON
        // and resumes with `--resume` when it wants more, so the app never continues
        // silently in the middle of somebody's pipeline.
        limits: bounds::limits_from_environment(&environment),
    };
    let observer = Arc::new(HeadlessObserver {
        stream_json: format == OutputFormat::StreamJson,
        ..HeadlessObserver::default()
    });
    observer.stream_event(
        "turn.started",
        &serde_json::json!({"session_id": session_id, "task_id": task_id}),
    );
    let turn_observer: Arc<dyn TurnObserver> = observer.clone();
    let cancellation = CancellationToken::new();
    let signal_cancellation = cancellation.clone();
    let signal_listener = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_cancellation.cancel();
        }
    });
    let outcome = match &resumed_from {
        Some((source, _)) => {
            driver
                .run_turn_continuing(
                    source,
                    run_request,
                    options,
                    Arc::clone(&turn_observer),
                    cancellation.clone(),
                )
                .await?
        }
        None => {
            driver
                .run_turn(
                    run_request,
                    options,
                    Arc::clone(&turn_observer),
                    cancellation,
                )
                .await?
        }
    };
    signal_listener.abort();
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
    // Stored before the writer is released, exactly like the interactive turn: the
    // admitted text comes back from the journal and is committed as reusable memory.
    let remembered = match &memory_principal {
        Some(principal) => memory::remember_input(Arc::clone(&store), principal, &session_id)
            .await
            .map(Some),
        None => Ok(None),
    };
    // And the turn itself, which is what answers "what did I ask you before?". It was
    // missing here while the interactive path had it, so a headless session recorded no
    // conversation at all and the history question had nothing to read.
    let turn = match &memory_principal {
        Some(principal) => memory::remember_turn(
            Arc::clone(&store),
            principal,
            &session_id,
            outcome.final_text.as_str(),
        )
        .await
        .map(Some),
        None => Ok(None),
    };
    let stored = memory_asset_of(&remembered);
    let stored_turn = memory_asset_of(&turn);
    // A headless run reports what it did not keep as well: a caller scripting this
    // reads the disposition instead of inferring it from a null. Two assets can be
    // written per turn - a directive and a turn record - so each has its own field
    // rather than one ambiguous id.
    let stored_disposition = memory_disposition_of(&remembered);
    let turn_disposition = memory_disposition_of(&turn);
    let memory_report = match (&memory_principal, &recall, &stored) {
        (Some(_), recall, stored) => serde_json::json!({
            "enabled": true,
            "recall": recall.as_ref().map(|found| serde_json::json!({
                "state": format!("{:?}", found.state).to_lowercase(),
                "hits": found.hits,
                "blocks": found.blocks,
                "message": found.message,
            })),
            "stored_asset_id": stored.clone(),
            "stored_disposition": stored_disposition,
            // The turn record is a second asset and gets its own field: one id could
            // only ever name one of the two, and which one it named was an accident of
            // write order.
            "turn_asset_id": stored_turn.clone(),
            "turn_disposition": turn_disposition,
            "error": remembered
                .as_ref()
                .err()
                .or_else(|| turn.as_ref().err())
                .map(ToString::to_string),
        }),
        (None, _, _) => serde_json::json!({"enabled": false}),
    };
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

    drop(driver);
    // Stop the extension processes this turn started, before the writer is released.
    if let Some(active) = active_extensions {
        active.shutdown().await;
    }
    drop(runtime);
    Arc::try_unwrap(store)
        .map_err(|_| {
            HarnessError::new(
                ErrorCode::StorageWriteFailed,
                "headless store consumers were not released",
            )
        })?
        .close()
        .await
        .map_err(StoreError::into_harness_error)?;
    acceptance_trace("writer_closed");

    if format == OutputFormat::Json {
        println!("{output}");
    } else if format == OutputFormat::StreamJson {
        observer.stream_event("run.terminal", &output);
    } else {
        for message in notices {
            println!("[info] {message}");
        }
        for message in auto_allowed {
            println!("[info] {message}");
        }
        println!("{}", output["response"].as_str().unwrap_or_default());
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
    Ok(ExitCode::from(exit))
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
pub(super) fn headless_approval_mode() -> ApprovalMode {
    ApprovalMode::None
}
