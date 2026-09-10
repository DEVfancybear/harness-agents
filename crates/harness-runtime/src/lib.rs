#![forbid(unsafe_code)]

//! Durable P2 runtime.  It owns the admission -> context -> frozen request ->
//! provider attempt sequence; providers never receive mutable session state.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};

use harness_providers::{
    CancellationToken, MessageRole, ModelProvider, NormalizedToolCall, ProviderError,
    ProviderMessage, ProviderRequest, assemble_stream,
};
use harness_session::{
    AdmitInputRequest, ContextBlock, ContextBuildRequest, ContextBuilder, RecoveryView,
    SessionService,
};
use harness_store_sqlite::{
    AgentStateRecord, CompositionSnapshotRecord, ContextCheckpointRecord, ContextPacketRecord,
    FrozenRequestRecord, ProviderAttemptRecord, RuntimeCommandRecord, RuntimeCommandState,
    SqliteStore, StoreError,
};
use harness_types::{
    AgentRunId, ContentHash, ErrorCode, InputId, SessionId, SourceAuthority, TaskId,
    WorkspaceObservation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Running,
    Paused,
    Completed,
    Failed,
    Canceled,
    Disposed,
}

impl AgentState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Disposed => "disposed",
        }
    }

    pub fn transition(self, next: Self) -> Result<Self, RuntimeError> {
        let allowed = matches!(
            (self, next),
            (Self::Idle, Self::Running)
                | (
                    Self::Running,
                    Self::Paused | Self::Completed | Self::Failed | Self::Canceled
                )
                | (Self::Paused, Self::Running | Self::Canceled)
                | (
                    Self::Completed | Self::Failed | Self::Canceled,
                    Self::Disposed
                )
        );
        if allowed {
            Ok(next)
        } else {
            Err(RuntimeError::new(
                ErrorCode::InvalidStateTransition,
                format!("invalid agent state transition {self:?} -> {next:?}"),
            ))
        }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub context_window_tokens: u64,
    pub output_reservation_tokens: u64,
    pub protocol_overhead_tokens: u64,
    pub safety_margin_tokens: u64,
    pub optional_token_budget: u64,
    pub max_attempts: u32,
    pub config_revision: u64,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            context_window_tokens: 8192,
            output_reservation_tokens: 1024,
            protocol_overhead_tokens: 128,
            safety_margin_tokens: 128,
            optional_token_budget: 2048,
            max_attempts: 3,
            config_revision: 1,
        }
    }
}

impl RuntimeConfig {
    pub fn validate(&self) -> Result<(), RuntimeError> {
        if self.context_window_tokens
            <= self
                .output_reservation_tokens
                .saturating_add(self.protocol_overhead_tokens)
                .saturating_add(self.safety_margin_tokens)
        {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "runtime context window is smaller than reserved output and overhead",
            ));
        }
        if self.max_attempts == 0 {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "runtime max_attempts must be positive",
            ));
        }
        if self.config_revision == 0 {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "runtime config revision must be positive",
            ));
        }
        Ok(())
    }
    #[must_use]
    pub fn with_max_attempts(mut self, value: u32) -> Self {
        self.max_attempts = value;
        self
    }
    #[must_use]
    pub fn with_config_revision(mut self, value: u64) -> Self {
        self.config_revision = value;
        self
    }
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub input_id: InputId,
    pub text: String,
    pub workspace: WorkspaceObservation,
    pub system_policy: String,
    pub continuation_context: Option<String>,
    pub tool_schemas: Vec<Value>,
}

impl RunRequest {
    #[must_use]
    pub fn new(
        session_id: SessionId,
        task_id: TaskId,
        input_id: InputId,
        text: impl Into<String>,
        workspace: WorkspaceObservation,
    ) -> Self {
        Self {
            session_id,
            task_id,
            input_id,
            text: text.into(),
            workspace,
            system_policy: "You are a careful coding agent.".to_owned(),
            continuation_context: None,
            tool_schemas: Vec::new(),
        }
    }
    #[must_use]
    pub fn with_system_policy(mut self, policy: impl Into<String>) -> Self {
        self.system_policy = policy.into();
        self
    }

    #[must_use]
    pub fn with_continuation_context(mut self, context: impl Into<String>) -> Self {
        self.continuation_context = Some(context.into());
        self
    }

    #[must_use]
    pub fn with_tool_schemas(mut self, tool_schemas: Vec<Value>) -> Self {
        self.tool_schemas = tool_schemas;
        self
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct RunResult {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub request_id: harness_types::RequestId,
    pub packet_id: harness_types::ContextPacketId,
    pub response: String,
    pub attempts: u32,
    pub tool_calls: Vec<NormalizedToolCall>,
    pub incomplete_tool_calls: bool,
}

#[derive(Clone, Debug)]
pub struct CompactionResult {
    pub packet: harness_types::ContextPacket,
    pub fallback_used: bool,
}

#[derive(Clone, Debug)]
pub struct ResumeReport {
    pub working_state: harness_types::WorkingState,
    pub packet: Option<harness_types::ContextPacket>,
    pub blocked: bool,
}

#[derive(Clone, Debug)]
pub struct OfflineReplayReport {
    pub blocked: bool,
    pub dispatch_count: u32,
    pub packets: Vec<harness_types::ContextPacket>,
    pub requests: Vec<FrozenRequestRecord>,
}

#[derive(Clone, Debug, Error)]
#[error("{code}: {message}")]
pub struct RuntimeError {
    code: ErrorCode,
    message: String,
}

impl RuntimeError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
}

impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}
impl From<ProviderError> for RuntimeError {
    fn from(error: ProviderError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}
impl From<harness_session::ContextError> for RuntimeError {
    fn from(error: harness_session::ContextError) -> Self {
        Self::new(error.code(), error.to_string())
    }
}

pub trait SummaryProvider: Send + Sync {
    fn summarize(&self, recovery: &RecoveryView) -> Result<String, RuntimeError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FailingSummaryProvider;
impl SummaryProvider for FailingSummaryProvider {
    fn summarize(&self, _recovery: &RecoveryView) -> Result<String, RuntimeError> {
        Err(RuntimeError::new(
            ErrorCode::ServiceUnavailable,
            "summary provider fixture failed",
        ))
    }
}

#[derive(Clone)]
pub struct RuntimeService {
    store: Arc<SqliteStore>,
    provider: Arc<dyn ModelProvider>,
    config: Arc<Mutex<RuntimeConfig>>,
    summarizer: Arc<dyn SummaryProvider>,
    last_attempts: Arc<AtomicU32>,
}

impl RuntimeService {
    #[must_use]
    pub fn new(
        store: Arc<SqliteStore>,
        provider: Arc<dyn ModelProvider>,
        config: RuntimeConfig,
    ) -> Self {
        Self {
            store,
            provider,
            config: Arc::new(Mutex::new(config)),
            summarizer: Arc::new(DefaultSummaryProvider),
            last_attempts: Arc::new(AtomicU32::new(0)),
        }
    }
    #[must_use]
    pub fn with_summarizer(mut self, summarizer: Arc<dyn SummaryProvider>) -> Self {
        self.summarizer = summarizer;
        self
    }
    pub fn update_config(&self, config: RuntimeConfig) {
        if let Ok(mut current) = self.config.lock() {
            *current = config;
        }
    }

    pub async fn run(&self, request: RunRequest) -> Result<RunResult, RuntimeError> {
        self.run_with_cancellation(request, CancellationToken::new())
            .await
    }

    #[allow(clippy::too_many_lines)]
    pub async fn run_with_cancellation(
        &self,
        request: RunRequest,
        cancellation: CancellationToken,
    ) -> Result<RunResult, RuntimeError> {
        let config = self
            .config
            .lock()
            .map_err(|_| {
                RuntimeError::new(ErrorCode::RuntimeBlocked, "runtime config lock is poisoned")
            })?
            .clone();
        config.validate()?;
        if request.text.trim().is_empty() {
            return Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "run text must not be empty",
            ));
        }
        let session = SessionService::new(Arc::clone(&self.store));
        let expected_sequence = self
            .store
            .session_summary(&request.session_id)
            .await?
            .map_or(1, |summary| summary.next_sequence);
        session
            .admit_input(AdmitInputRequest {
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                input_id: request.input_id.clone(),
                expected_sequence,
                authority: SourceAuthority::User,
                raw_text: request.text.clone(),
                workspace: request.workspace.clone(),
                initial_plan_items: Vec::new(),
            })
            .await?;
        let agent_run_id = AgentRunId::generate();
        self.record_agent(&agent_run_id, &request, AgentState::Idle, 0)
            .await?;
        self.record_agent(&agent_run_id, &request, AgentState::Running, 1)
            .await?;
        let command_id = harness_types::RuntimeCommandId::generate();
        self.store
            .enqueue_runtime_command(RuntimeCommandRecord {
                command_id: command_id.clone(),
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                state: RuntimeCommandState::Pending,
                attempts: 0,
                owner_generation: 0,
                payload: json!({"input_id": request.input_id, "text": request.text}),
                last_error: None,
            })
            .await?;
        let command = self
            .store
            .claim_runtime_command(&command_id, config.max_attempts)
            .await?;
        self.last_attempts.store(command.attempts, Ordering::SeqCst);
        let recovery = session.recover(&request.session_id).await?;
        let built = self.build_context(&request, recovery)?;
        let capabilities = self.provider.capabilities();
        let composition_content = json!({"config_revision": config.config_revision, "provider_id": capabilities.provider_id, "model": capabilities.model, "packet_checkpoint": built.packet.checkpoint_id, "tool_schemas": request.tool_schemas});
        let composition_id = harness_types::CompositionSnapshotId::generate();
        let composition = CompositionSnapshotRecord {
            snapshot_id: composition_id.clone(),
            session_id: request.session_id.clone(),
            task_id: request.task_id.clone(),
            revision: built.packet.through_event_seq,
            content_hash: ContentHash::from_canonical_json(&composition_content)
                .map_err(|error| RuntimeError::new(error.code(), error.to_string()))?,
            content: composition_content,
        };
        self.store.persist_composition_snapshot(composition).await?;
        let provider_request = ProviderRequest::new(
            harness_types::RequestId::generate(),
            capabilities.model.clone(),
            vec![
                ProviderMessage::new(MessageRole::System, request.system_policy.clone()),
                ProviderMessage::new(MessageRole::User, built.packet.content.clone()),
            ],
        )
        .with_tool_schemas(request.tool_schemas.clone());
        self.store
            .persist_context_packet(ContextPacketRecord {
                packet: built.packet.clone(),
                composition_snapshot_id: Some(composition_id.clone()),
                omitted_optional: built.omitted_optional.clone(),
                degradation: built.degradation.clone(),
            })
            .await?;
        let request_json = serde_json::to_value(&provider_request).map_err(|_| {
            RuntimeError::new(
                ErrorCode::InvalidPayload,
                "provider request cannot be serialized",
            )
        })?;
        let frozen = FrozenRequestRecord {
            request_id: provider_request.request_id.clone(),
            packet_id: built.packet.packet_id.clone(),
            composition_snapshot_id: Some(composition_id),
            session_id: request.session_id.clone(),
            task_id: request.task_id.clone(),
            content_hash: ContentHash::from_canonical_json(&request_json)
                .map_err(|error| RuntimeError::new(error.code(), error.to_string()))?,
            request_json,
            provider_id: capabilities.provider_id,
            model: capabilities.model,
            config_revision: config.config_revision,
        };
        self.store.persist_frozen_request(frozen).await?;

        let mut final_response = None;
        let mut attempts = command.attempts;
        let mut last_error = None;
        while attempts <= config.max_attempts {
            if cancellation.is_cancelled() {
                last_error = Some(RuntimeError::new(
                    ErrorCode::ProviderCanceled,
                    "run canceled before provider dispatch",
                ));
                break;
            }
            let attempt_number = attempts.max(1);
            let result = self
                .provider
                .stream(provider_request.clone(), cancellation.clone())
                .await;
            match result {
                Ok(events) => {
                    let assembled = assemble_stream(&events)?;
                    let response_hash = ContentHash::from_canonical_json(
                        &serde_json::to_value(&assembled).map_err(|_| {
                            RuntimeError::new(
                                ErrorCode::InvalidPayload,
                                "provider response cannot be serialized",
                            )
                        })?,
                    )
                    .map_err(|error| RuntimeError::new(error.code(), error.to_string()))?;
                    self.store
                        .persist_provider_attempt(ProviderAttemptRecord {
                            attempt_id: harness_types::ProviderAttemptId::generate(),
                            request_id: provider_request.request_id.clone(),
                            session_id: request.session_id.clone(),
                            task_id: request.task_id.clone(),
                            attempt_number,
                            state: "completed".to_owned(),
                            events: serde_json::to_value(&events).unwrap_or_else(|_| json!([])),
                            response_hash: Some(response_hash),
                            error: None,
                        })
                        .await?;
                    final_response = Some(assembled);
                    attempts = attempt_number;
                    break;
                }
                Err(error) => {
                    let canceled = error.code() == ErrorCode::ProviderCanceled;
                    self.store
                        .persist_provider_attempt(ProviderAttemptRecord {
                            attempt_id: harness_types::ProviderAttemptId::generate(),
                            request_id: provider_request.request_id.clone(),
                            session_id: request.session_id.clone(),
                            task_id: request.task_id.clone(),
                            attempt_number,
                            state: if canceled { "canceled" } else { "failed" }.to_owned(),
                            events: json!([]),
                            response_hash: None,
                            error: Some(error.to_string()),
                        })
                        .await?;
                    last_error = Some(RuntimeError::from(error));
                    attempts = attempt_number;
                    if canceled {
                        break;
                    }
                    if attempts >= config.max_attempts {
                        break;
                    }
                    let _ = self
                        .store
                        .bump_runtime_command_attempt(
                            &command_id,
                            command.owner_generation,
                            config.max_attempts,
                        )
                        .await?;
                    attempts = attempts.saturating_add(1);
                }
            }
        }
        self.last_attempts.store(attempts, Ordering::SeqCst);
        if let Some(response) = final_response {
            let mut payload = Map::new();
            payload.insert(
                "request_id".to_owned(),
                Value::String(provider_request.request_id.to_string()),
            );
            payload.insert("text".to_owned(), Value::String(response.text.clone()));
            payload.insert(
                "tool_calls".to_owned(),
                serde_json::to_value(&response.tool_calls).map_err(|_| {
                    RuntimeError::new(
                        ErrorCode::InvalidPayload,
                        "provider tool calls cannot be serialized",
                    )
                })?,
            );
            payload.insert(
                "incomplete_tool_calls".to_owned(),
                Value::Bool(response.incomplete_tool_calls),
            );
            let _ = session
                .append_runtime_event(
                    &request.session_id,
                    &request.task_id,
                    "model.response",
                    payload,
                    false,
                )
                .await?;
            self.store
                .complete_runtime_command(
                    &command_id,
                    command.owner_generation,
                    RuntimeCommandState::Completed,
                    None,
                )
                .await?;
            self.record_agent(
                &agent_run_id,
                &request,
                AgentState::Completed,
                u64::from(attempts) + 1,
            )
            .await?;
            Ok(RunResult {
                session_id: request.session_id,
                task_id: request.task_id,
                request_id: provider_request.request_id,
                packet_id: built.packet.packet_id,
                response: response.text,
                attempts,
                tool_calls: response.tool_calls,
                incomplete_tool_calls: response.incomplete_tool_calls,
            })
        } else {
            let error = last_error.unwrap_or_else(|| {
                RuntimeError::new(ErrorCode::RetryExhausted, "provider retry budget exhausted")
            });
            let state = if error.code() == ErrorCode::ProviderCanceled {
                RuntimeCommandState::Canceled
            } else {
                RuntimeCommandState::Pending
            };
            let _ = self
                .store
                .complete_runtime_command(
                    &command_id,
                    command.owner_generation,
                    state,
                    Some(&error.to_string()),
                )
                .await;
            let _ = self
                .record_agent(
                    &agent_run_id,
                    &request,
                    if error.code() == ErrorCode::ProviderCanceled {
                        AgentState::Canceled
                    } else {
                        AgentState::Failed
                    },
                    u64::from(attempts) + 1,
                )
                .await;
            Err(error)
        }
    }

    pub async fn compact(&self, session_id: &SessionId) -> Result<CompactionResult, RuntimeError> {
        let session = SessionService::new(Arc::clone(&self.store));
        let task_id = self.store.session_task(session_id).await?.ok_or_else(|| {
            RuntimeError::new(ErrorCode::InvalidPayload, "session does not exist")
        })?;
        let mut started = Map::new();
        started.insert(
            "reason".to_owned(),
            Value::String("context_budget".to_owned()),
        );
        let _ = session
            .append_runtime_event(session_id, &task_id, "compaction.started", started, false)
            .await?;
        let recovery = session.recover(session_id).await?;
        let summary = self.summarizer.summarize(&recovery);
        let fallback_used = summary.is_err();
        let system_policy = summary.unwrap_or_else(|_| "Deterministic WorkingState fallback; preserve every mandatory instruction and correction.".to_owned());
        let request = RunRequest::new(
            session_id.clone(),
            task_id.clone(),
            InputId::generate(),
            recovery
                .instruction_texts
                .first()
                .cloned()
                .unwrap_or_else(|| "continue task".to_owned()),
            recovery.working_state.workspace.clone(),
        )
        .with_system_policy(system_policy);
        let built = self.build_context(&request, recovery)?;
        let content_json = json!({"packet": built.packet.content, "fallback_used": fallback_used, "task_id": task_id});
        let checkpoint = ContextCheckpointRecord {
            checkpoint_id: built.packet.checkpoint_id.clone(),
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            through_sequence: built.packet.through_event_seq,
            revision: built.packet.through_event_seq,
            content_hash: ContentHash::from_canonical_json(&content_json)
                .map_err(|error| RuntimeError::new(error.code(), error.to_string()))?,
            content: content_json,
        };
        let expected_last = self
            .store
            .next_sequence(session_id)
            .await?
            .saturating_sub(1);
        self.store
            .write_context_checkpoint_cas(checkpoint, expected_last)
            .await?;
        self.store
            .persist_context_packet(ContextPacketRecord {
                packet: built.packet.clone(),
                composition_snapshot_id: None,
                omitted_optional: built.omitted_optional,
                degradation: built.degradation,
            })
            .await?;
        let mut completed = Map::new();
        completed.insert(
            "checkpoint_id".to_owned(),
            Value::String(built.packet.checkpoint_id.clone()),
        );
        let _ = session
            .append_runtime_event(
                session_id,
                &task_id,
                "compaction.completed",
                completed,
                false,
            )
            .await?;
        Ok(CompactionResult {
            packet: built.packet,
            fallback_used,
        })
    }

    pub async fn resume(&self, session_id: &SessionId) -> Result<ResumeReport, RuntimeError> {
        let session = SessionService::new(Arc::clone(&self.store));
        let recovery = session.recover(session_id).await?;
        let packet = self
            .store
            .latest_context_packet(session_id)
            .await?
            .map(|record| record.packet);
        Ok(ResumeReport {
            working_state: recovery.working_state,
            packet,
            blocked: false,
        })
    }

    pub async fn continue_task(
        &self,
        source_session_id: &SessionId,
        request: RunRequest,
    ) -> Result<RunResult, RuntimeError> {
        let source_task = self
            .store
            .session_task(source_session_id)
            .await?
            .ok_or_else(|| {
                RuntimeError::new(ErrorCode::InvalidPayload, "source session does not exist")
            })?;
        if source_task != request.task_id {
            return Err(RuntimeError::new(
                ErrorCode::IdempotencyConflict,
                "continuation task does not match source session",
            ));
        }
        let request =
            if let Some(packet) = self.store.latest_context_packet(source_session_id).await? {
                request.with_continuation_context(packet.packet.content)
            } else {
                request
            };
        let result = self.run(request).await?;
        self.store
            .record_continuation_link(source_session_id, &result.session_id, &result.task_id)
            .await?;
        Ok(result)
    }

    pub async fn offline_replay(
        &self,
        session_id: &SessionId,
    ) -> Result<OfflineReplayReport, RuntimeError> {
        let session = SessionService::new(Arc::clone(&self.store));
        let blocked = session.recover(session_id).await.is_err();
        let packets = self
            .store
            .list_context_packets(session_id)
            .await?
            .into_iter()
            .map(|record| record.packet)
            .collect();
        let requests = self.store.list_frozen_requests(session_id).await?;
        Ok(OfflineReplayReport {
            blocked,
            dispatch_count: 0,
            packets,
            requests,
        })
    }

    #[allow(clippy::unused_async)]
    pub async fn last_command_attempts(&self) -> Result<u32, RuntimeError> {
        Ok(self.last_attempts.load(Ordering::SeqCst))
    }

    fn build_context(
        &self,
        request: &RunRequest,
        recovery: RecoveryView,
    ) -> Result<harness_session::ContextBuildResult, RuntimeError> {
        let config = self
            .config
            .lock()
            .map_err(|_| {
                RuntimeError::new(ErrorCode::RuntimeBlocked, "runtime config lock is poisoned")
            })?
            .clone();
        let continuation = request
            .continuation_context
            .as_ref()
            .map(|text| {
                vec![ContextBlock::mandatory(
                    "continuation-context",
                    harness_session::ContextBlockKind::RecentTail,
                    text.clone(),
                )]
            })
            .unwrap_or_default();
        ContextBuilder::new()
            .build(ContextBuildRequest {
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                checkpoint_id: format!("checkpoint-{}", recovery.replayed_through_sequence),
                through_event_seq: recovery.replayed_through_sequence,
                recovery,
                system_policy: request.system_policy.clone(),
                project_rules: Vec::<ContextBlock>::new(),
                optional_blocks: Vec::new(),
                recent_tail: continuation,
                context_window_tokens: config.context_window_tokens,
                output_reservation_tokens: config.output_reservation_tokens,
                protocol_overhead_tokens: config.protocol_overhead_tokens,
                safety_margin_tokens: config.safety_margin_tokens,
                optional_token_budget: config.optional_token_budget,
                memory_versions: Vec::new(),
            })
            .map_err(RuntimeError::from)
    }

    async fn record_agent(
        &self,
        agent_run_id: &AgentRunId,
        request: &RunRequest,
        state: AgentState,
        revision: u64,
    ) -> Result<(), RuntimeError> {
        let generation = self.store.fence()?.generation;
        self.store
            .record_agent_state(AgentStateRecord {
                agent_run_id: agent_run_id.clone(),
                session_id: request.session_id.clone(),
                task_id: request.task_id.clone(),
                state: state.as_str().to_owned(),
                generation,
                revision,
                detail: json!({}),
            })
            .await
            .map_err(RuntimeError::from)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct DefaultSummaryProvider;
impl SummaryProvider for DefaultSummaryProvider {
    fn summarize(&self, recovery: &RecoveryView) -> Result<String, RuntimeError> {
        Ok(format!(
            "WorkingState revision {} is authoritative; preserve mandatory instructions.",
            recovery.working_state.revision
        ))
    }
}
