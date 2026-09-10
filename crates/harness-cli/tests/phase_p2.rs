use std::{
    process::{Command, Output},
    sync::Arc,
};

use harness_providers::{
    CancellationToken, DeepSeekAdapter, MessageRole, MockProvider, ModelCapabilities,
    ModelProvider, ProviderMessage, ProviderRequest, ProviderStreamEvent, SseDecoder,
    StaticCredentialResolver, assemble_stream,
};
use harness_runtime::{
    AgentState, FailingSummaryProvider, RunRequest, RuntimeConfig, RuntimeService,
};
use harness_session::{
    ContextBlock, ContextBlockKind, ContextBuildRequest, ContextBuilder, SessionService,
};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, EventId, HostId, InputId, P0_SCHEMA_VERSION, PlanItem,
    ProducerIdentity, ProjectId, SessionId, SourceAuthority, TaskId, WorkspaceObservation,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "phase-p2".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"phase p2 workspace"),
    }
}

fn request(session_id: SessionId, task_id: TaskId, text: &str) -> RunRequest {
    RunRequest::new(session_id, task_id, InputId::generate(), text, workspace())
        .with_system_policy("You are a careful coding agent.")
}

async fn writer(temp: &TempDir) -> Arc<SqliteStore> {
    Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer should open"),
    )
}

async fn close_writer(store: Arc<SqliteStore>) {
    let store = Arc::try_unwrap(store).expect("store consumers must be released");
    store.close().await.expect("writer closes");
}

fn provider_request() -> ProviderRequest {
    ProviderRequest::new(
        harness_types::RequestId::generate(),
        "deepseek-chat",
        vec![ProviderMessage::new(MessageRole::User, "hello")],
    )
}

fn run_ha(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(arguments)
        .output()
        .expect("ha binary should execute")
}

#[test]
fn p2_s01_runtime_contracts_and_state_machine_are_versioned() {
    let config = RuntimeConfig::default();
    config.validate().expect("default runtime config is valid");
    assert_eq!(
        AgentState::Idle.transition(AgentState::Running).unwrap(),
        AgentState::Running
    );
    assert_eq!(
        AgentState::Running.transition(AgentState::Paused).unwrap(),
        AgentState::Paused
    );
    assert_eq!(
        AgentState::Idle
            .transition(AgentState::Disposed)
            .unwrap_err()
            .code(),
        ErrorCode::InvalidStateTransition
    );
    assert_eq!(harness_store_sqlite::RUNTIME_SCHEMA_VERSION, 1);
}

#[tokio::test]
async fn p2_s02_provider_streams_and_deepseek_sse_adapter_are_normalized() {
    let mock = MockProvider::scripted(vec![
        ProviderStreamEvent::started(),
        ProviderStreamEvent::text("xin "),
        ProviderStreamEvent::text("chào"),
        ProviderStreamEvent::tool_delta("call-1", "read_file", "{\"path\":"),
        ProviderStreamEvent::completed("stop"),
    ]);
    let events = mock
        .stream(provider_request(), CancellationToken::new())
        .await
        .expect("mock stream");
    let response = assemble_stream(&events).expect("normalized response");
    assert_eq!(response.text, "xin chào");
    assert!(response.incomplete_tool_calls);

    let mut decoder = SseDecoder::new();
    let first = decoder
        .feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hel")
        .expect("first split accepted");
    assert!(first.is_empty());
    let second = decoder
        .feed(b"lo\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n")
        .expect("second split accepted");
    assert!(second.iter().any(ProviderStreamEvent::is_completed));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let server = tokio::spawn(async move {
        let (mut socket, _peer) = listener.accept().await.expect("fixture accepts");
        let mut request_bytes = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.expect("fixture reads");
            if read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&chunk[..read]);
            if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("fixture writes");
        request_bytes
    });
    let adapter = DeepSeekAdapter::new(
        format!("http://{address}/chat/completions"),
        Arc::new(StaticCredentialResolver::new("fixture-secret")),
        ModelCapabilities::deepseek_fixture(),
    )
    .expect("adapter config");
    let events = adapter
        .stream(provider_request(), CancellationToken::new())
        .await
        .expect("fixture adapter stream");
    let response = assemble_stream(&events).expect("fixture response");
    assert_eq!(response.text, "ok");
    let request_bytes = server.await.expect("fixture server");
    let request_text = String::from_utf8(request_bytes).expect("fixture request UTF-8");
    assert!(request_text.contains("Bearer fixture-secret"));
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("fixture-secret")
    );
}

#[tokio::test]
async fn p2_s03_mandatory_context_overflow_pauses_with_explanation() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    service
        .admit_input(harness_session::AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "mandatory instruction that cannot be dropped".to_owned(),
            workspace: workspace(),
            initial_plan_items: Vec::<PlanItem>::new(),
        })
        .await
        .unwrap();
    let recovery = service.recover(&session_id).await.unwrap();
    let build = ContextBuilder::new().build(ContextBuildRequest {
        session_id,
        task_id,
        checkpoint_id: "checkpoint-overflow".to_owned(),
        through_event_seq: recovery.replayed_through_sequence,
        recovery,
        system_policy: "policy ".repeat(100),
        project_rules: vec![],
        optional_blocks: vec![],
        recent_tail: vec![],
        context_window_tokens: 16,
        output_reservation_tokens: 4,
        protocol_overhead_tokens: 4,
        safety_margin_tokens: 4,
        optional_token_budget: 0,
        memory_versions: vec![],
    });
    let error = build.expect_err("mandatory overflow must pause");
    assert_eq!(error.code(), ErrorCode::MandatoryContextOverflow);
    drop(service);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c18_unclassified_instruction_is_mandatory_after_compaction() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    service
        .admit_input(harness_session::AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "giữ nguyên public API dù compact".to_owned(),
            workspace: workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .unwrap();
    let recovery = service.recover(&session_id).await.unwrap();
    let packet = ContextBuilder::new()
        .build(ContextBuildRequest {
            session_id,
            task_id,
            checkpoint_id: "checkpoint-instruction".to_owned(),
            through_event_seq: recovery.replayed_through_sequence,
            recovery,
            system_policy: "policy".to_owned(),
            project_rules: vec![],
            optional_blocks: vec![],
            recent_tail: vec![],
            context_window_tokens: 512,
            output_reservation_tokens: 32,
            protocol_overhead_tokens: 8,
            safety_margin_tokens: 8,
            optional_token_budget: 64,
            memory_versions: vec![],
        })
        .unwrap();
    assert!(
        packet
            .packet
            .content
            .contains("giữ nguyên public API dù compact")
    );
    assert!(
        packet
            .mandatory_block_ids
            .iter()
            .any(|id| id.contains("instruction"))
    );
    drop(service);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c19_project_rule_with_zero_relevance_is_admitted() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let service = SessionService::new(Arc::clone(&store));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    service
        .admit_input(harness_session::AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: task_id.clone(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "work on parser".to_owned(),
            workspace: workspace(),
            initial_plan_items: Vec::new(),
        })
        .await
        .unwrap();
    let recovery = service.recover(&session_id).await.unwrap();
    let packet = ContextBuilder::new()
        .build(ContextBuildRequest {
            session_id,
            task_id,
            checkpoint_id: "checkpoint-rule".to_owned(),
            through_event_seq: recovery.replayed_through_sequence,
            recovery,
            system_policy: "policy".to_owned(),
            project_rules: vec![ContextBlock::mandatory(
                "project-rule-public-api",
                ContextBlockKind::ProjectRule,
                "do not change the public API",
            )],
            optional_blocks: vec![],
            recent_tail: vec![],
            context_window_tokens: 512,
            output_reservation_tokens: 32,
            protocol_overhead_tokens: 8,
            safety_margin_tokens: 8,
            optional_token_budget: 64,
            memory_versions: vec![],
        })
        .unwrap();
    assert!(
        packet
            .packet
            .content
            .contains("do not change the public API")
    );
    drop(service);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_s04_request_is_committed_before_provider_and_replayed_exactly() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("model answer"));
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        provider.clone(),
        RuntimeConfig::default(),
    );
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let result = runtime
        .run(request(session_id.clone(), task_id, "fix parser"))
        .await
        .unwrap();
    assert_eq!(provider.call_count(), 1);
    let requests = store.list_frozen_requests(&session_id).await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].request_id, result.request_id);
    assert_eq!(requests[0].packet_id, result.packet_id);
    let replay = runtime.offline_replay(&session_id).await.unwrap();
    assert_eq!(replay.dispatch_count, 0);
    assert_eq!(replay.requests[0].content_hash, requests[0].content_hash);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_s05_summary_failure_uses_deterministic_working_state_fallback() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("done"));
    let runtime = RuntimeService::new(Arc::clone(&store), provider, RuntimeConfig::default())
        .with_summarizer(Arc::new(FailingSummaryProvider));
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    runtime
        .run(request(
            session_id.clone(),
            task_id,
            "preserve the parser objective",
        ))
        .await
        .unwrap();
    let compacted = runtime.compact(&session_id).await.unwrap();
    assert!(compacted.fallback_used);
    assert!(
        compacted
            .packet
            .content
            .contains("preserve the parser objective")
    );
    assert!(
        store
            .latest_context_checkpoint(&session_id)
            .await
            .unwrap()
            .is_some()
    );
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c04_five_compactions_preserve_mandatory_state_and_corrections() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("done"));
    let runtime = RuntimeService::new(Arc::clone(&store), provider, RuntimeConfig::default());
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    runtime
        .run(request(
            session_id.clone(),
            task_id.clone(),
            "keep all acceptance criteria",
        ))
        .await
        .unwrap();
    let session = SessionService::new(Arc::clone(&store));
    session
        .record_decision(&session_id, &task_id, "decision A", None)
        .await
        .unwrap();
    let a = session
        .recover(&session_id)
        .await
        .unwrap()
        .working_state
        .decision_refs
        .last()
        .cloned()
        .unwrap();
    session
        .record_decision(&session_id, &task_id, "decision B", Some(a))
        .await
        .unwrap();
    for _ in 0..5 {
        let result = runtime.compact(&session_id).await.unwrap();
        assert!(
            result
                .packet
                .content
                .contains("keep all acceptance criteria")
        );
        assert!(result.packet.content.contains("decision B"));
    }
    let recovered = session.recover(&session_id).await.unwrap();
    assert_eq!(recovered.working_state.superseded_decision_refs.len(), 1);
    drop(session);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c09_decision_supersession_survives_context_and_resume() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("done"));
    let runtime = RuntimeService::new(Arc::clone(&store), provider, RuntimeConfig::default());
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    runtime
        .run(request(
            session_id.clone(),
            task_id.clone(),
            "follow current decision",
        ))
        .await
        .unwrap();
    let session = SessionService::new(Arc::clone(&store));
    let a = session
        .record_decision(&session_id, &task_id, "A", None)
        .await
        .unwrap();
    session
        .record_decision(&session_id, &task_id, "B", Some(a))
        .await
        .unwrap();
    let compacted = runtime.compact(&session_id).await.unwrap();
    assert!(compacted.packet.content.contains('B'));
    let resumed = runtime.resume(&session_id).await.unwrap();
    assert!(resumed.packet.unwrap().content.contains('B'));
    assert_eq!(resumed.working_state.superseded_decision_refs.len(), 1);
    drop(session);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c06_new_session_continuation_reuses_task_checkpoint_and_scope() {
    let temp = TempDir::new().unwrap();
    let task_id = TaskId::generate();
    let old_session = SessionId::generate();
    let provider = Arc::new(MockProvider::text("first"));
    {
        let store = writer(&temp).await;
        let runtime = RuntimeService::new(
            Arc::clone(&store),
            provider.clone(),
            RuntimeConfig::default(),
        );
        runtime
            .run(request(
                old_session.clone(),
                task_id.clone(),
                "original task",
            ))
            .await
            .unwrap();
        drop(runtime);
        close_writer(store).await;
    }
    let store = writer(&temp).await;
    let runtime = RuntimeService::new(Arc::clone(&store), provider, RuntimeConfig::default());
    let new_session = SessionId::generate();
    let result = runtime
        .continue_task(
            &old_session,
            request(
                new_session.clone(),
                task_id.clone(),
                "continue the same task",
            ),
        )
        .await
        .unwrap();
    assert_eq!(result.task_id, task_id);
    assert_eq!(result.session_id, new_session);
    let lineage = store
        .continuation_link(&new_session)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(lineage.source_session_id, old_session);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_c14_offline_replay_dispatches_nothing_and_matches_frozen_packet() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let first_provider = Arc::new(MockProvider::text("recorded response"));
    let runtime = RuntimeService::new(Arc::clone(&store), first_provider, RuntimeConfig::default());
    let session_id = SessionId::generate();
    runtime
        .run(request(session_id.clone(), TaskId::generate(), "replay me"))
        .await
        .unwrap();
    let offline_provider = Arc::new(MockProvider::text("must not run"));
    let offline_runtime = RuntimeService::new(
        Arc::clone(&store),
        offline_provider.clone(),
        RuntimeConfig::default(),
    );
    let replay = offline_runtime.offline_replay(&session_id).await.unwrap();
    assert_eq!(replay.dispatch_count, 0);
    assert!(!replay.packets.is_empty());
    assert_eq!(offline_provider.call_count(), 0);
    drop(offline_runtime);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_k10_config_change_is_boundary_scoped_and_frozen_request_is_immutable() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("done"));
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig::default().with_config_revision(1),
    );
    let first_session = SessionId::generate();
    runtime
        .run(request(
            first_session.clone(),
            TaskId::generate(),
            "revision one",
        ))
        .await
        .unwrap();
    runtime.update_config(RuntimeConfig::default().with_config_revision(2));
    let second_session = SessionId::generate();
    runtime
        .run(request(
            second_session.clone(),
            TaskId::generate(),
            "revision two",
        ))
        .await
        .unwrap();
    let first = store.list_frozen_requests(&first_session).await.unwrap();
    let second = store.list_frozen_requests(&second_session).await.unwrap();
    assert_eq!(first[0].config_revision, 1);
    assert_eq!(second[0].config_revision, 2);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_k11_unknown_critical_event_blocks_resume_but_allows_offline_inspection() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::text("done"));
    let runtime = RuntimeService::new(Arc::clone(&store), provider, RuntimeConfig::default());
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    runtime
        .run(request(session_id.clone(), task_id, "critical event test"))
        .await
        .unwrap();
    let sequence = store.next_sequence(&session_id).await.unwrap();
    let payload = json!({"unknown": true});
    let payload = payload.as_object().unwrap().clone();
    store
        .append_event_for_test(EventEnvelope {
            schema_version: P0_SCHEMA_VERSION,
            event_id: EventId::generate(),
            session_id: session_id.clone(),
            seq: sequence,
            event_type: "future.critical.event".to_owned(),
            producer: ProducerIdentity {
                plugin_id: "fixture".to_owned(),
                implementation_version: "1".to_owned(),
            },
            authority: SourceAuthority::RuntimeObserved,
            correlation_id: None,
            causation_id: None,
            continuity_critical: true,
            payload: payload.clone(),
            payload_hash: ContentHash::from_canonical_json(&Value::Object(payload)).unwrap(),
        })
        .await
        .unwrap();
    let error = runtime
        .resume(&session_id)
        .await
        .expect_err("resume must block");
    assert_eq!(error.code(), ErrorCode::UnknownCriticalEvent);
    let offline = runtime.offline_replay(&session_id).await.unwrap();
    assert!(offline.blocked);
    assert!(!offline.packets.is_empty());
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_s06_cancel_and_retry_are_bounded_and_recoverable() {
    let temp = TempDir::new().unwrap();
    let store = writer(&temp).await;
    let provider = Arc::new(MockProvider::delayed_text("never", 200));
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        provider,
        RuntimeConfig::default().with_max_attempts(2),
    );
    let cancellation = CancellationToken::new();
    let cancel_clone = cancellation.clone();
    let session_id = SessionId::generate();
    let task_id = TaskId::generate();
    let runtime_clone = runtime.clone();
    let handle = tokio::spawn(async move {
        runtime_clone
            .run_with_cancellation(
                request(session_id.clone(), task_id, "cancel me"),
                cancel_clone,
            )
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    cancellation.cancel();
    let error = handle
        .await
        .unwrap()
        .expect_err("cancellation should stop run");
    assert_eq!(error.code(), ErrorCode::ProviderCanceled);
    assert!(runtime.last_command_attempts().await.unwrap() <= 2);
    drop(runtime);
    close_writer(store).await;
}

#[tokio::test]
async fn p2_s07_cli_runs_resumes_continues_inspects_and_replays_offline() {
    let temp = TempDir::new().unwrap();
    let data_dir = temp.path().to_str().unwrap();
    let run = run_ha(&[
        "run",
        "--data-dir",
        data_dir,
        "--text",
        "cli task",
        "--json",
    ]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let run_json: Value = serde_json::from_slice(&run.stdout).unwrap();
    let session_id = run_json["session_id"].as_str().unwrap();
    let task_id = run_json["task_id"].as_str().unwrap();
    let resume = run_ha(&[
        "resume",
        "--data-dir",
        data_dir,
        "--session-id",
        session_id,
        "--json",
    ]);
    assert!(
        resume.status.success(),
        "{}",
        String::from_utf8_lossy(&resume.stderr)
    );
    let context = run_ha(&[
        "context",
        "inspect",
        "--data-dir",
        data_dir,
        "--session-id",
        session_id,
        "--json",
    ]);
    assert!(
        context.status.success(),
        "{}",
        String::from_utf8_lossy(&context.stderr)
    );
    let replay = run_ha(&[
        "session",
        "replay",
        "--data-dir",
        data_dir,
        "--session-id",
        session_id,
        "--offline",
        "--json",
    ]);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let continuation = run_ha(&[
        "continue",
        "--data-dir",
        data_dir,
        "--task-id",
        task_id,
        "--text",
        "continue",
        "--json",
    ]);
    assert!(
        continuation.status.success(),
        "{}",
        String::from_utf8_lossy(&continuation.stderr)
    );
}

#[test]
fn p2_provider_decoder_rejects_malformed_frame() {
    let mut decoder = SseDecoder::new();
    let error = decoder.feed(b"data: {not-json}\n\n").unwrap_err();
    assert_eq!(error.code(), ErrorCode::ProviderProtocol);
}
