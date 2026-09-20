//! Headless one-turn launch.
//!
//! A headless turn runs through the same application service the interactive app
//! uses: same runtime, same store, same tool gate. Results go to stdout, logs stay
//! on stderr, and no raw terminal mode is ever enabled. An unconfigured provider is
//! reported as an actionable error instead of a fabricated answer.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use harness_providers::{CancellationToken, DeepSeekAdapter, ModelCapabilities, ModelProvider};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, StoreError, WriterOpenOptions};
use harness_tools::{
    ApprovalMode, ToolExecutionService, TurnDriver, TurnLimits, TurnObserver, TurnOptions,
    TurnProgress, coding_tool_schemas, observe_workspace,
};
use harness_types::{ErrorCode, HarnessError, HostId, InputId, SessionId, TaskId};

use super::bootstrap::{self, LaunchRequest};
use super::extensions;
use super::memory;
use super::paths::{HostPlatform, LaunchEnvironment};
use super::project;
use super::service::{EnvironmentCredential, resolve_provider, validate_credential_file};

/// A validated single-turn headless request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeadlessRequest {
    pub prompt: String,
    pub json: bool,
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
}

/// Progress is not rendered in headless mode; the final answer is printed once.
struct SilentObserver;

impl TurnObserver for SilentObserver {
    fn observe(&self, _progress: TurnProgress) {}
}

/// Debug-only acceptance trace for a child that is killed at a hard deadline.
/// Normal users never see it, and release artifacts do not contain the switch.
fn acceptance_trace(#[cfg_attr(not(debug_assertions), allow(unused_variables))] stage: &str) {
    #[cfg(debug_assertions)]
    if std::env::var_os("HA_TEST_TRACE_HEADLESS").is_some() {
        eprintln!("HA_HEADLESS_PHASE {stage}");
    }
}

/// Run one headless turn.
///
/// The body is a linear sequence: resolve, run exactly one turn, shut down. It is
/// long because it wires real components, not because it branches.
#[allow(clippy::too_many_lines)]
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
        caller_dir,
        platform: HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    })?;
    acceptance_trace("bootstrap_resolved");
    // Resolve the provider before opening anything: an unconfigured environment
    // must fail fast with instructions and must not create state.
    let config = resolve_provider(&environment, &context.paths.data_dir)
        .map_err(|message| HarnessError::new(ErrorCode::ServiceUnavailable, message))?;
    // A key the app saved is read by the resolver at call time. Prove that here,
    // before a store is opened or a turn is admitted, so a saved-but-unreadable
    // key fails with an actionable message instead of mid-turn.
    validate_credential_file(&environment, &context.paths.data_dir)
        .map_err(|message| HarnessError::new(ErrorCode::SecretNotGranted, message))?;

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
    let capabilities = ModelCapabilities {
        provider_id: "deepseek".to_owned(),
        model: config.model.clone(),
        supports_streaming: true,
        supports_tools: true,
        fixture: false,
    };
    let provider: Arc<dyn ModelProvider> = Arc::new(
        DeepSeekAdapter::new(
            config.endpoint.clone(),
            Arc::new(EnvironmentCredential::new(
                config.credential_variable(),
                context.paths.data_dir.clone(),
            )),
            capabilities,
        )
        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?,
    );
    let runtime = Arc::new(RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig::default(),
    ));
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
        Some(active) => {
            ToolExecutionService::new(Arc::clone(&store)).with_external(active.dispatcher())
        }
        None => ToolExecutionService::new(Arc::clone(&store)),
    };
    let driver = TurnDriver::new(Arc::clone(&runtime), tools);
    let driver = match &active_extensions {
        Some(active) => driver.with_external(active.tools()),
        None => driver,
    };
    let tool_schemas = match &active_extensions {
        Some(active) => {
            let mut schemas = coding_tool_schemas();
            schemas.extend(active.tools().schemas());
            schemas
        }
        None => coding_tool_schemas(),
    };
    let run_request = RunRequest::new(
        session_id.clone(),
        task_id,
        InputId::generate(),
        request.prompt.clone(),
        observation,
    )
    .with_tool_schemas(tool_schemas);
    let mut recall = None;
    let run_request = match &memory_principal {
        Some(principal) => {
            match memory::recall(Arc::clone(&store), principal, &request.prompt).await {
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
        approvals: ApprovalMode::None,
        limits: TurnLimits::default(),
    };
    let outcome = match &resumed_from {
        Some((source, _)) => {
            driver
                .run_turn_continuing(
                    source,
                    run_request,
                    options,
                    Arc::new(SilentObserver),
                    CancellationToken::new(),
                )
                .await?
        }
        None => {
            driver
                .run_turn(
                    run_request,
                    options,
                    Arc::new(SilentObserver),
                    CancellationToken::new(),
                )
                .await?
        }
    };
    acceptance_trace("turn_finished");
    // Stored before the writer is released, exactly like the interactive turn: the
    // admitted text comes back from the journal and is committed as reusable memory.
    let stored = match &memory_principal {
        Some(principal) => memory::remember_input(Arc::clone(&store), principal, &session_id)
            .await
            .map(|stored| stored.map(|asset_id| asset_id.as_str().to_owned())),
        None => Ok(None),
    };
    let memory_report = match (&memory_principal, &recall, &stored) {
        (Some(_), recall, stored) => serde_json::json!({
            "enabled": true,
            "recall": recall.as_ref().map(|found| serde_json::json!({
                "state": format!("{:?}", found.state).to_lowercase(),
                "hits": found.hits,
                "blocks": found.blocks,
                "message": found.message,
            })),
            "stored_asset_id": stored.as_ref().ok().and_then(Clone::clone),
            "error": stored.as_ref().err().map(ToString::to_string),
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
    let output = serde_json::json!({
        "schema_version": 1,
        "session_id": outcome.session_id,
        "task_id": outcome.task_id,
        "input_id": outcome.input_id,
        "response": outcome.final_text,
        "steps": outcome.steps,
        "tool_calls": outcome.tool_calls,
        "stop": format!("{:?}", outcome.stop).to_lowercase(),
        "approvals": "none",
        "fixture": false,
        "memory": memory_report,
        "extensions": extensions_report,
        "resumed_from": resumed_from
            .as_ref()
            .map(|(source, _)| source.as_str().to_owned()),
    });

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

    if request.json {
        println!("{output}");
    } else {
        println!("{}", output["response"].as_str().unwrap_or_default());
    }
    Ok(ExitCode::SUCCESS)
}
