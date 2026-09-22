//! M2 — provider protocol and incremental stream.
//!
//! A06/A07 run here against a real loopback HTTP fixture that writes each SSE
//! part on request, so streaming order is measured instead of assumed. The
//! fixture never sleeps to fake timing: a barrier holds the terminal frame until
//! the test has observed the text.

use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use harness_providers::{
    CapabilityClaim, CapabilityMatrix, DeepSeekAdapter, MessageRole, MockProvider,
    ModelCapabilities, ModelProvider, ProviderMessage, ProviderRequest, ProviderStreamEvent,
    StaticCredentialResolver, assemble_stream, collect_events, validate_transcript,
};
use harness_runtime::{RunRequest, RuntimeConfig, RuntimeService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    ContentHash, ErrorCode, HostId, InputId, ProjectId, RequestId, SessionId, TaskId,
    WorkspaceObservation,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

// ---------------------------------------------------------------------------
// The fixture HTTP provider
// ---------------------------------------------------------------------------

/// One scripted HTTP response served by the fixture.
#[derive(Clone, Debug)]
struct FakeResponse {
    status: u16,
    retry_after: Option<u64>,
    /// Body written in order, one socket write per part.
    parts: Vec<Vec<u8>>,
    /// Hold the connection after this part index until the test releases it.
    barrier_after: Option<usize>,
    /// Keep the connection open after the last part (for cancellation).
    keep_open: bool,
}

impl FakeResponse {
    fn ok(parts: Vec<String>) -> Self {
        Self {
            status: 200,
            retry_after: None,
            parts: parts.into_iter().map(String::into_bytes).collect(),
            barrier_after: None,
            keep_open: false,
        }
    }

    fn status(status: u16, retry_after: Option<u64>) -> Self {
        Self {
            status,
            retry_after,
            parts: Vec::new(),
            barrier_after: None,
            keep_open: false,
        }
    }

    fn with_barrier(mut self, part: usize) -> Self {
        self.barrier_after = Some(part);
        self
    }

    fn keeping_open(mut self) -> Self {
        self.keep_open = true;
        self
    }
}

/// A loopback provider that serves one scripted response per request, in order
/// (the last response repeats), and records every request it received.
struct FakeProvider {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    barrier_hits: tokio::sync::mpsc::UnboundedReceiver<()>,
    release: Arc<tokio::sync::Notify>,
    ready: tokio::sync::oneshot::Receiver<()>,
    finished: Arc<AtomicBool>,
}

impl FakeProvider {
    #[allow(clippy::too_many_lines)] // one accept loop, kept in one place on purpose
    async fn start(responses: Vec<FakeResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let (barrier_hit, barrier_hits) = tokio::sync::mpsc::unbounded_channel::<()>();
        let release = Arc::new(tokio::sync::Notify::new());
        let server_release = Arc::clone(&release);
        let (ready_sender, ready) = tokio::sync::oneshot::channel::<()>();
        let finished = Arc::new(AtomicBool::new(false));
        let server_finished = Arc::clone(&finished);
        tokio::spawn(async move {
            let _ = ready_sender.send(());
            let mut served = 0_usize;
            loop {
                let Ok((mut socket, _peer)) = listener.accept().await else {
                    break;
                };
                // Read the request head, then exactly the declared body, so the
                // captured request is complete and the client is not reset.
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                let mut head_end = None;
                while head_end.is_none() {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    head_end = request
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|index| index + 4);
                }
                let Some(head_end) = head_end else {
                    // A readiness probe connects and closes without a request.
                    continue;
                };
                let head = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
                let content_length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                while request.len() < head_end + content_length {
                    let Ok(read) = socket.read(&mut chunk).await else {
                        break;
                    };
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                captured
                    .lock()
                    .expect("request log")
                    .push(String::from_utf8_lossy(&request).into_owned());
                let response = responses
                    .get(served)
                    .or_else(|| responses.last())
                    .cloned()
                    .expect("the fixture has a scripted response");
                served += 1;
                if response.status != 200 {
                    let mut head = format!(
                        "HTTP/1.1 {} Fixture\r\nContent-Length: 0\r\nConnection: close\r\n",
                        response.status
                    );
                    if let Some(seconds) = response.retry_after {
                        let _ = std::fmt::Write::write_fmt(
                            &mut head,
                            format_args!("Retry-After: {seconds}\r\n"),
                        );
                    }
                    head.push_str("\r\n");
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.shutdown().await;
                    continue;
                }
                let length: usize = response.parts.iter().map(Vec::len).sum();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
                );
                let _ = socket.write_all(head.as_bytes()).await;
                for (index, part) in response.parts.iter().enumerate() {
                    if socket.write_all(part).await.is_err() {
                        break;
                    }
                    if socket.flush().await.is_err() {
                        break;
                    }
                    if response.barrier_after == Some(index) {
                        let _ = barrier_hit.send(());
                        server_release.notified().await;
                    }
                }
                if response.keep_open {
                    // Hold the connection until the client goes away, so a cancel
                    // is what ends the stream.
                    let mut tail = [0_u8; 64];
                    while let Ok(read) = socket.read(&mut tail).await {
                        if read == 0 {
                            break;
                        }
                    }
                }
                let _ = socket.shutdown().await;
                if served >= responses.len() {
                    server_finished.store(true, Ordering::SeqCst);
                }
            }
        });
        Self {
            address,
            requests,
            barrier_hits,
            release,
            ready,
            finished,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}/chat/completions", self.address)
    }

    /// Wait until the fixture is accepting connections.
    async fn wait_ready(&mut self) {
        let _ = (&mut self.ready).await;
        // Confirm the listener really accepts a connection before the test
        // sends its real request. The old loop returned after twenty probes
        // whether or not any of them connected, so under load the request could
        // race a listener that was not accepting yet and fail as
        // `error sending request for url`. Now readiness is asserted, with a
        // short sleep after the first success so the accept loop is scheduled.
        let mut accepted = false;
        for _ in 0..300 {
            if std::net::TcpStream::connect(self.address).is_ok() {
                accepted = true;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(accepted, "fixture listener must be accepting connections");
    }

    /// Wait until a barrier part has been written.
    async fn await_barrier(&mut self) {
        self.barrier_hits
            .recv()
            .await
            .expect("the fixture reaches its barrier");
    }

    fn release_barrier(&self) {
        self.release.notify_one();
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("request log").len()
    }

    fn request_text(&self, index: usize) -> String {
        self.requests.lock().expect("request log")[index].clone()
    }
}

fn adapter(provider: &FakeProvider, token: &str) -> DeepSeekAdapter {
    DeepSeekAdapter::with_timeouts(
        provider.endpoint(),
        Arc::new(StaticCredentialResolver::new(token)),
        ModelCapabilities::deepseek_fixture(),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(10),
    )
    .expect("adapter config")
}

fn provider_request() -> ProviderRequest {
    ProviderRequest::new(
        RequestId::generate(),
        "deepseek-chat",
        vec![ProviderMessage::new(MessageRole::User, "hello")],
    )
}

/// The SSE body every streaming assertion starts from: text, two interleaved
/// tool calls, a usage-only frame, then the terminal marker.
fn multicall_parts() -> Vec<String> {
    vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n".to_owned(),
        "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n".to_owned(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":1,\"id\":\"call_b\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\":\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n".to_owned(),
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"arguments\":\"\\\".\\\"}\"}}]},\"finish_reason\":null}]}\n\n".to_owned(),
        "data: {\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":5,\"total_tokens\":12}}\n\n".to_owned(),
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n".to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ]
}

/// Whether a provider failure is this host refusing or truncating a loopback
/// connection rather than a property of the stream under test.
///
/// Measured 21-22/09/2026 and again 23/09/2026: the refusal reaches a real child
/// process as `error sending request`, sometimes with a ` for url` suffix and
/// sometimes without one, and the same window can surface as
/// `error decoding response body` when the connection is cut mid-response. The
/// older key named only the suffixed form, so these tests failed immediately
/// instead of using the bounded retry that exists for exactly this condition.
fn is_loopback_refusal(error: &harness_providers::ProviderError) -> bool {
    let text = error.to_string();
    text.contains("error sending request") || text.contains("error decoding response body")
}

async fn collect(adapter: &DeepSeekAdapter) -> Vec<ProviderStreamEvent> {
    // Retry only a refused or truncated loopback connection: this Windows host
    // intermittently refuses a connection to a listener that is already bound and
    // accepting, even when this exact test runs alone in the milestone closure
    // (measured 21-22/09/2026). The refusal never reaches the fixture, so a retry
    // cannot consume a scripted response, and every other error still fails at
    // once.
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        let result = collect_events(adapter.stream_events(
            provider_request(),
            harness_providers::CancellationToken::new(),
        ))
        .await;
        match result {
            Ok(events) => return events,
            Err(error) if attempt < 12 && is_loopback_refusal(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(
                    50 * (1_u64 << attempt.min(6)),
                ))
                .await;
            }
            Err(error) => panic!("the fixture stream is valid: {error}"),
        }
    }
}

/// Collect the stream of a test that expects an error.
///
/// Like [`collect`], only a refused or truncated loopback connection is retried;
/// every other error is returned so the test's own assertion decides. The refusal
/// never reaches the fixture, so a retry cannot consume a scripted response.
async fn stream_error(adapter: &DeepSeekAdapter) -> harness_providers::ProviderError {
    let mut attempt = 0_u32;
    loop {
        attempt += 1;
        let result = collect_events(adapter.stream_events(
            provider_request(),
            harness_providers::CancellationToken::new(),
        ))
        .await;
        match result {
            Err(error) if attempt < 12 && is_loopback_refusal(&error) => {
                tokio::time::sleep(std::time::Duration::from_millis(
                    50 * (1_u64 << attempt.min(6)),
                ))
                .await;
            }
            Err(error) => return error,
            Ok(_) => panic!("the fixture stream must fail"),
        }
    }
}

fn workspace() -> WorkspaceObservation {
    WorkspaceObservation {
        project_id: ProjectId::generate(),
        worktree_id: "m2-test".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        observed_fingerprint: ContentHash::from_bytes(b"m2 test workspace"),
    }
}

async fn writer() -> (tempfile::TempDir, Arc<SqliteStore>) {
    let temp = tempfile::tempdir().expect("temp dir");
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(temp.path(), HostId::generate()))
            .await
            .expect("writer opens"),
    );
    (temp, store)
}

// ---------------------------------------------------------------------------
// A06 — SSE partial and multiple tool calls
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a06_sse_multicall_and_usage_survive_chunk_boundaries() {
    // Every part is written as its own socket write, so frames arrive split.
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(multicall_parts())]).await;
    provider.wait_ready().await;
    let events = collect(&adapter(&provider, "fixture-secret")).await;

    let response = assemble_stream(&events).expect("assembly");
    assert_eq!(response.text, "hello");
    assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    assert!(!response.incomplete_tool_calls);
    assert!(response.is_dispatchable());
    assert_eq!(response.tool_calls.len(), 2);
    let mut calls = response.tool_calls.clone();
    calls.sort_by(|left, right| left.call_id.cmp(&right.call_id));
    assert_eq!(calls[0].call_id, "call_a");
    assert_eq!(calls[0].name, "read_file");
    assert_eq!(calls[0].arguments, "{\"path\":\"a.rs\"}");
    assert_eq!(calls[1].call_id, "call_b");
    assert_eq!(calls[1].name, "list_files");
    assert_eq!(calls[1].arguments, "{\"path\":\".\"}");
    assert!(
        events.iter().any(|event| matches!(
            event,
            ProviderStreamEvent::Usage {
                total_tokens: 12,
                ..
            }
        )),
        "a usage-only frame must not be dropped: {events:?}"
    );
}

#[tokio::test]
async fn a06_missing_terminal_and_truncated_arguments_are_not_dispatchable() {
    // No [DONE] and no finish_reason, with one complete call.
    let parts = vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".to_owned(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
    ];
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(parts)]).await;
    provider.wait_ready().await;
    let response =
        assemble_stream(&collect(&adapter(&provider, "fixture-secret")).await).expect("assembly");
    assert_eq!(response.finish_reason, None);
    assert!(
        !response.is_dispatchable(),
        "a stream without a terminal must never be dispatched"
    );

    // A truncated JSON argument is reported as incomplete instead of executed.
    let truncated = vec![
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ];
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(truncated)]).await;
    provider.wait_ready().await;
    let response =
        assemble_stream(&collect(&adapter(&provider, "fixture-secret")).await).expect("assembly");
    assert!(response.incomplete_tool_calls);
    assert!(!response.is_dispatchable());
}

#[tokio::test]
async fn a06_id_conflict_is_typed_and_never_dispatched() {
    let parts = vec![
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
        concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_b\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{}\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        )
        .to_owned(),
    ];
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(parts)]).await;
    provider.wait_ready().await;
    let error = stream_error(&adapter(&provider, "fixture-secret")).await;
    assert_eq!(error.code(), ErrorCode::ProviderProtocol);
    assert!(error.to_string().contains("call slot"), "{error}");
}

// ---------------------------------------------------------------------------
// A07 — provider errors, retry and cancellation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a07_401_is_not_retried_and_transient_is_bounded() {
    // 401: one attempt, typed authority error.
    let mut provider = FakeProvider::start(vec![FakeResponse::status(401, None)]).await;
    provider.wait_ready().await;
    let (_temp, store) = writer().await;
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(adapter(&provider, "fixture-secret")),
        RuntimeConfig::default().with_max_attempts(3),
    );
    let error = runtime
        .run(RunRequest::new(
            SessionId::generate(),
            TaskId::generate(),
            InputId::generate(),
            "401 must not be retried",
            workspace(),
        ))
        .await
        .expect_err("a 401 fails the run");
    assert_eq!(error.code(), ErrorCode::MissingAuthority);
    assert_eq!(
        provider.request_count(),
        1,
        "a permanent provider failure is attempted once"
    );
    assert!(!provider.finished.load(Ordering::SeqCst));
    drop(runtime);
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .unwrap();

    // 503 twice, then a valid stream: bounded retry inside max_attempts.
    let mut responses = vec![
        FakeResponse::status(503, Some(0)),
        FakeResponse::status(503, Some(0)),
        FakeResponse::ok(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"recovered\"},\"finish_reason\":null}]}\n\n".to_owned(),
            "data: [DONE]\n\n".to_owned(),
        ]),
    ];
    responses[0].retry_after = Some(0);
    let mut provider = FakeProvider::start(responses).await;
    provider.wait_ready().await;
    let (_temp, store) = writer().await;
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(adapter(&provider, "fixture-secret")),
        RuntimeConfig::default().with_max_attempts(3),
    );
    let result = runtime
        .run(RunRequest::new(
            SessionId::generate(),
            TaskId::generate(),
            InputId::generate(),
            "transient is retried",
            workspace(),
        ))
        .await
        .expect("the third attempt succeeds");
    assert_eq!(result.response, "recovered");
    assert_eq!(result.attempts, 3);
    assert_eq!(provider.request_count(), 3);
    drop(runtime);
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn a07_errors_never_carry_the_token() {
    const SENTINEL: &str = "sk-m2-sentinel-must-not-leak";
    let mut provider = FakeProvider::start(vec![FakeResponse::status(401, None)]).await;
    provider.wait_ready().await;
    let (_temp, store) = writer().await;
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(adapter(&provider, SENTINEL)),
        RuntimeConfig::default().with_max_attempts(1),
    );
    let error = runtime
        .run(RunRequest::new(
            SessionId::generate(),
            TaskId::generate(),
            InputId::generate(),
            "no token in errors",
            workspace(),
        ))
        .await
        .expect_err("the run fails");
    let rendered = format!("{error}");
    assert!(!rendered.contains(SENTINEL), "{rendered}");
    // The fixture did receive the token in the authorization header, so the
    // assertion above is about the error path, not about a request that never had one.
    let request = provider.request_text(0);
    assert!(
        request.contains(SENTINEL),
        "the request carries the bearer token"
    );
    drop(runtime);
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .unwrap();
}

#[tokio::test]
async fn a07_cancel_settles_the_stream() {
    let parts = vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n"
            .to_owned(),
    ];
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(parts).keeping_open()]).await;
    provider.wait_ready().await;
    let cancellation = harness_providers::CancellationToken::new();
    let mut stream = adapter(&provider, "fixture-secret")
        .stream_events(provider_request(), cancellation.clone());
    let first = futures_util::StreamExt::next(&mut stream)
        .await
        .expect("one event")
        .expect("the text delta arrives");
    assert_eq!(first, ProviderStreamEvent::text("first"));
    cancellation.cancel();
    // The stream settles with a typed cancellation, and the socket is closed.
    let next = futures_util::StreamExt::next(&mut stream).await;
    match next {
        Some(Err(error)) => assert_eq!(error.code(), ErrorCode::ProviderCanceled),
        None => {}
        Some(Ok(event)) => panic!("a canceled stream must not deliver {event:?}"),
    }
}

// ---------------------------------------------------------------------------
// M2-01 — typed messages and capability claims
// ---------------------------------------------------------------------------

#[test]
fn m2_01_transcript_validation_refuses_orphan_and_duplicate_calls() {
    let valid = vec![
        ProviderMessage::new(MessageRole::User, "hi"),
        ProviderMessage::assistant_with_calls(
            "asking",
            vec![harness_providers::ProviderToolCall::new(
                "call_a",
                "read_file",
                "{}",
            )],
        ),
        ProviderMessage::tool_result("call_a", "ok"),
    ];
    validate_transcript(&valid).expect("a paired transcript is valid");

    let orphan = vec![ProviderMessage::tool_result("call_missing", "ok")];
    assert_eq!(
        validate_transcript(&orphan)
            .expect_err("an orphan result is refused")
            .code(),
        ErrorCode::ProviderProtocol
    );

    let duplicate_call = vec![
        ProviderMessage::assistant_with_calls(
            "asking",
            vec![harness_providers::ProviderToolCall::new(
                "call_a",
                "read_file",
                "{}",
            )],
        ),
        ProviderMessage::assistant_with_calls(
            "asking again",
            vec![harness_providers::ProviderToolCall::new(
                "call_a",
                "read_file",
                "{}",
            )],
        ),
    ];
    assert_eq!(
        validate_transcript(&duplicate_call)
            .expect_err("a reused call id is refused")
            .code(),
        ErrorCode::ProviderProtocol
    );

    let answered_twice = vec![
        ProviderMessage::assistant_with_calls(
            "asking",
            vec![harness_providers::ProviderToolCall::new(
                "call_a",
                "read_file",
                "{}",
            )],
        ),
        ProviderMessage::tool_result("call_a", "ok"),
        ProviderMessage::tool_result("call_a", "ok again"),
    ];
    assert_eq!(
        validate_transcript(&answered_twice)
            .expect_err("one call is answered once")
            .code(),
        ErrorCode::ProviderProtocol
    );
}

#[test]
fn m2_01_capability_unsupported_is_refused_unknown_is_allowed() {
    let request = ProviderRequest::new(
        RequestId::generate(),
        "deepseek-chat",
        vec![ProviderMessage::new(MessageRole::User, "hi")],
    )
    .with_tool_schemas(vec![serde_json::json!({"type": "function"})]);

    let unsupported = CapabilityMatrix {
        provider_id: "fixture".to_owned(),
        model: "no-tools".to_owned(),
        streaming: CapabilityClaim::Supported,
        tools: CapabilityClaim::Unsupported,
        images: CapabilityClaim::Unknown,
    };
    assert_eq!(
        unsupported
            .validate(&request)
            .expect_err("an explicitly unsupported parameter is refused")
            .code(),
        ErrorCode::IncompatibleService
    );

    let unknown = CapabilityMatrix {
        tools: CapabilityClaim::Unknown,
        ..unsupported.clone()
    };
    unknown
        .validate(&request)
        .expect("unknown is not a refusal and is not proof either");

    let documented = CapabilityMatrix::deepseek_documented("deepseek-flash");
    assert_eq!(documented.tools, CapabilityClaim::Supported);
    documented.validate(&request).expect("documented tools");
}

// ---------------------------------------------------------------------------
// M2-04 — conformance and the streaming barrier
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m2_04_text_arrives_before_the_terminal_barrier() {
    let parts = vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"earlier\"},\"finish_reason\":null}]}\n\n"
            .to_owned(),
        "data: [DONE]\n\n".to_owned(),
    ];
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(parts).with_barrier(0)]).await;
    provider.wait_ready().await;
    let mut stream = adapter(&provider, "fixture-secret").stream_events(
        provider_request(),
        harness_providers::CancellationToken::new(),
    );
    let first = futures_util::StreamExt::next(&mut stream)
        .await
        .expect("one event")
        .expect("text arrives");
    assert_eq!(first, ProviderStreamEvent::text("earlier"));
    // The fixture is still holding the terminal frame; the client has the text.
    provider.await_barrier().await;
    provider.release_barrier();
    let second = futures_util::StreamExt::next(&mut stream)
        .await
        .expect("terminal event")
        .expect("terminal arrives");
    assert!(second.is_completed());
}

#[tokio::test]
async fn m2_04_conformance_mock_and_adapter_agree() {
    let parts = multicall_parts();
    let mut provider = FakeProvider::start(vec![FakeResponse::ok(parts.clone())]).await;
    provider.wait_ready().await;
    let over_http = collect(&adapter(&provider, "fixture-secret")).await;

    // The same script as a scripted mock: the normalizer must agree.
    let mock_events = harness_providers::collect_events(
        MockProvider::scripted(mock_script(&parts)).stream_events(
            provider_request(),
            harness_providers::CancellationToken::new(),
        ),
    )
    .await
    .expect("mock stream");

    let http_response = assemble_stream(&over_http).expect("http assembly");
    let mock_response = assemble_stream(&mock_events).expect("mock assembly");
    assert_eq!(http_response.text, mock_response.text);
    assert_eq!(http_response.finish_reason, mock_response.finish_reason);
    assert_eq!(http_response.tool_calls, mock_response.tool_calls);
    assert_eq!(
        http_response.incomplete_tool_calls,
        mock_response.incomplete_tool_calls
    );
    assert_eq!(http_response, mock_response);
}

fn mock_script(parts: &[String]) -> Vec<ProviderStreamEvent> {
    // Decode the same frames through the public decoder so the mock script and the
    // HTTP body cannot drift apart.
    let mut decoder = harness_providers::SseDecoder::new();
    let mut events = Vec::new();
    for part in parts {
        events.extend(decoder.feed(part.as_bytes()).expect("fixture frame"));
    }
    events.extend(decoder.finish().expect("fixture end"));
    events
}
