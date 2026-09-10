use std::{path::PathBuf, sync::Arc};

use harness_runtime::{RunRequest, RunResult, RuntimeService};
use harness_types::{ErrorCode, HarnessError};

use crate::{ToolExecutionService, ToolExecutionView, ToolRequest};

/// One P2 provider response plus the P3 actions it requested. This phase does
/// not recursively send tool results back to a model; that loop boundary is
/// explicit rather than silently pretending a completed agent turn.
#[derive(Clone, Debug)]
pub struct CodingLoopResult {
    pub runtime: RunResult,
    pub executions: Vec<ToolExecutionView>,
}

/// Directional bridge: P2 normalizes provider calls and P3 executes complete
/// calls through its one gate. `harness-runtime` intentionally does not depend
/// back on this crate.
#[derive(Clone)]
pub struct CodingLoopService {
    runtime: Arc<RuntimeService>,
    tools: ToolExecutionService,
}

impl CodingLoopService {
    #[must_use]
    pub fn new(runtime: Arc<RuntimeService>, tools: ToolExecutionService) -> Self {
        Self { runtime, tools }
    }

    pub async fn run_once(
        &self,
        request: RunRequest,
        workspace_root: impl Into<PathBuf>,
        actor_id: impl Into<String>,
        grant_approvals: bool,
    ) -> Result<CodingLoopResult, HarnessError> {
        let workspace_root = workspace_root.into();
        let actor_id = actor_id.into();
        let runtime = self
            .runtime
            .run(request)
            .await
            .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
        if runtime.incomplete_tool_calls {
            return Err(HarnessError::new(
                ErrorCode::ProviderProtocol,
                "provider emitted incomplete tool arguments; no tool intent was created",
            ));
        }
        let mut executions = Vec::new();
        for call in &runtime.tool_calls {
            let action = crate::CodingToolAction::from_provider_call(&call.name, &call.arguments)?;
            let prepared = self
                .tools
                .prepare(ToolRequest::new(
                    runtime.session_id.clone(),
                    runtime.task_id.clone(),
                    actor_id.clone(),
                    workspace_root.clone(),
                    action,
                ))
                .await?;
            let approval = if grant_approvals {
                Some(self.tools.approve(&prepared).await?)
            } else {
                None
            };
            executions.push(self.tools.execute(prepared, approval).await?);
        }
        Ok(CodingLoopResult {
            runtime,
            executions,
        })
    }
}
