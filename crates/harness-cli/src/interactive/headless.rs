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
use harness_types::{ErrorCode, HarnessError, HostId, InputId, ProjectId, SessionId, TaskId};

use super::bootstrap::{self, LaunchRequest};
use super::paths::{HostPlatform, LaunchEnvironment};
use super::service::{EnvironmentCredential, resolve_provider};

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

/// Run one headless turn.
///
/// The body is a linear sequence: resolve, run exactly one turn, shut down. It is
/// long because it wires real components, not because it branches.
#[allow(clippy::too_many_lines)]
pub async fn run(request: HeadlessRequest) -> Result<ExitCode, HarnessError> {
    if let Some(session) = &request.resume {
        return Err(HarnessError::new(
            ErrorCode::ServiceUnavailable,
            format!(
                "resuming session {session} in a headless turn arrives with HA_LAUNCH H05; no state was changed"
            ),
        ));
    }
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
    // Resolve the provider before opening anything: an unconfigured environment
    // must fail fast with instructions and must not create state.
    let config = resolve_provider(&environment)
        .map_err(|message| HarnessError::new(ErrorCode::ServiceUnavailable, message))?;

    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            context.project_store_dir(),
            HostId::generate(),
        ))
        .await
        .map_err(StoreError::into_harness_error)?,
    );
    let observation = observe_workspace(ProjectId::generate(), &context.project.root)?;
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
                config.credential_variable.clone(),
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
    let driver = TurnDriver::new(
        Arc::clone(&runtime),
        ToolExecutionService::new(Arc::clone(&store)),
    );
    let run_request = RunRequest::new(
        SessionId::generate(),
        TaskId::generate(),
        InputId::generate(),
        request.prompt.clone(),
        observation,
    )
    .with_tool_schemas(coding_tool_schemas());
    let options = TurnOptions {
        workspace_root: context.project.root.clone(),
        actor_id: "headless.user".to_owned(),
        // There is nobody to ask: a gated action fails closed.
        approvals: ApprovalMode::None,
        limits: TurnLimits::default(),
    };
    let outcome = driver
        .run_turn(
            run_request,
            options,
            Arc::new(SilentObserver),
            CancellationToken::new(),
        )
        .await?;
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
    });

    drop(driver);
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

    if request.json {
        println!("{output}");
    } else {
        println!("{}", output["response"].as_str().unwrap_or_default());
    }
    Ok(ExitCode::SUCCESS)
}
