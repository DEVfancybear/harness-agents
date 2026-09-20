//! Incremental provider streaming (`HA_LAUNCH` H04, gate G1).
//!
//! The P2 boundary returns a fully collected vector of events, so a caller
//! cannot show text before the whole response arrived. This module adds an
//! additive incremental boundary next to it: the existing `ModelProvider` trait,
//! its callers and its tests stay untouched, and `collect_events` bridges back to
//! the buffered shape for callers that still want one vector.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::{Stream, StreamExt};
use harness_types::ErrorCode;
use tokio::sync::mpsc;

use crate::{
    CancellationToken, DeepSeekAdapter, MockProvider, ProviderError, ProviderFuture,
    ProviderRequest, ProviderStreamEvent, SseDecoder,
};

/// Incremental event stream from one provider call.
pub type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send>>;

/// Bridge a buffered future into the incremental shape.
///
/// This is the default for a provider that can only answer in one piece: the
/// events arrive together instead of progressively, which callers can detect.
pub(crate) fn bridge_buffered(future: ProviderFuture) -> ProviderEventStream {
    let (sender, mut receiver) = mpsc::channel::<Result<ProviderStreamEvent, ProviderError>>(16);
    tokio::spawn(async move {
        match future.await {
            Ok(events) => {
                for event in events {
                    if sender.send(Ok(event)).await.is_err() {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error)).await;
            }
        }
    });
    Box::pin(futures_util::stream::poll_fn(move |context| {
        receiver.poll_recv(context)
    }))
}

/// Collect an incremental stream into the buffered shape P2 callers expect.
pub async fn collect_events(
    mut stream: ProviderEventStream,
) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    Ok(events)
}

/// Incremental implementation for the deterministic mock provider.
pub(crate) fn mock_stream(
    provider: &MockProvider,
    request: ProviderRequest,
    cancellation: CancellationToken,
) -> ProviderEventStream {
    {
        let script = Arc::clone(&provider.script);
        let calls = Arc::clone(&provider.calls);
        let delay_ms = provider.delay_ms;
        // Bounded channel: the consumer applies backpressure instead of letting a
        // long script buffer without limit.
        let (sender, mut receiver) = mpsc::channel::<Result<ProviderStreamEvent, ProviderError>>(4);
        tokio::spawn(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if cancellation.is_cancelled() {
                let _ = sender
                    .send(Err(ProviderError::new(
                        ErrorCode::ProviderCanceled,
                        "provider call canceled before dispatch",
                    )))
                    .await;
                return;
            }
            let mut events = (*script).clone();
            if let Some(ProviderStreamEvent::Started { request_id }) = events.first_mut() {
                *request_id = request.request_id;
            }
            for event in events {
                if cancellation.is_cancelled() {
                    let _ = sender
                        .send(Err(ProviderError::new(
                            ErrorCode::ProviderCanceled,
                            "provider call canceled",
                        )))
                        .await;
                    return;
                }
                if delay_ms > 0 {
                    tokio::select! {
                        () = tokio::time::sleep(Duration::from_millis(delay_ms)) => {},
                        () = cancellation.cancelled() => {
                            let _ = sender.send(Err(ProviderError::new(
                                ErrorCode::ProviderCanceled,
                                "provider call canceled",
                            ))).await;
                            return;
                        }
                    }
                }
                if sender.send(Ok(event)).await.is_err() {
                    return;
                }
            }
        });
        Box::pin(futures_util::stream::poll_fn(move |context| {
            receiver.poll_recv(context)
        }))
    }
}

/// Incremental implementation for the `DeepSeek` adapter.
///
/// The transport loop is deliberately linear: request, decode, forward. It is
/// long but has no branching business logic.
#[allow(clippy::too_many_lines)]
pub(crate) fn adapter_stream(
    provider: &DeepSeekAdapter,
    request: ProviderRequest,
    cancellation: CancellationToken,
) -> ProviderEventStream {
    {
        let endpoint = provider.endpoint.clone();
        let credentials = Arc::clone(&provider.credentials);
        let client = provider.client.clone();
        let (sender, mut receiver) =
            mpsc::channel::<Result<ProviderStreamEvent, ProviderError>>(16);
        tokio::spawn(async move {
            if cancellation.is_cancelled() {
                let _ = sender
                    .send(Err(ProviderError::new(
                        ErrorCode::ProviderCanceled,
                        "provider call canceled before dispatch",
                    )))
                    .await;
                return;
            }
            let token = match credentials.resolve() {
                Ok(token) => token,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let mut body = serde_json::json!({
                "model": request.model,
                "messages": crate::wire_messages(&request.messages),
                "stream": true,
                "temperature": request.temperature,
                "thinking": crate::thinking_disabled(),
            });
            if !request.tool_schemas.is_empty()
                && let Some(object) = body.as_object_mut()
            {
                object.insert(
                    "tools".to_owned(),
                    serde_json::Value::Array(request.tool_schemas.clone()),
                );
            }
            let response = tokio::select! {
                result = client.post(&endpoint).bearer_auth(token).json(&body).send() => match result {
                    Ok(response) => response,
                    Err(error) => {
                        let _ = sender.send(Err(ProviderError::new(
                            ErrorCode::ProviderProtocol,
                            format!("provider request failed: {error}"),
                        ))).await;
                        return;
                    }
                },
                () = cancellation.cancelled() => {
                    let _ = sender.send(Err(ProviderError::new(
                        ErrorCode::ProviderCanceled,
                        "provider request canceled",
                    ))).await;
                    return;
                }
            };
            if !response.status().is_success() {
                let _ = sender
                    .send(Err(ProviderError::new(
                        ErrorCode::ProviderProtocol,
                        format!("provider returned HTTP {}", response.status()),
                    )))
                    .await;
                return;
            }
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::new();
            loop {
                let next = tokio::select! {
                    item = stream.next() => item,
                    () = cancellation.cancelled() => {
                        let _ = sender.send(Err(ProviderError::new(
                            ErrorCode::ProviderCanceled,
                            "provider stream canceled",
                        ))).await;
                        return;
                    }
                };
                let Some(chunk) = next else { break };
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        let _ = sender
                            .send(Err(ProviderError::new(
                                ErrorCode::ProviderProtocol,
                                format!("provider stream failed: {error}"),
                            )))
                            .await;
                        return;
                    }
                };
                match decoder.feed(&chunk) {
                    Ok(events) => {
                        for event in events {
                            if sender.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                        return;
                    }
                }
            }
            match decoder.finish() {
                Ok(events) => {
                    for event in events {
                        if sender.send(Ok(event)).await.is_err() {
                            return;
                        }
                    }
                }
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                }
            }
        });
        Box::pin(futures_util::stream::poll_fn(move |context| {
            receiver.poll_recv(context)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::collect_events;
    use crate::{
        CancellationToken, DeepSeekAdapter, MessageRole, MockProvider, ModelCapabilities,
        ModelProvider, ProviderMessage, ProviderRequest, ProviderStreamEvent,
        StaticCredentialResolver,
    };
    use futures_util::StreamExt;
    use harness_types::RequestId;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Wait until the fixture listener really accepts connections.
    ///
    /// Binding a port only puts the socket into listen, and this environment can
    /// refuse a connection to a freshly bound listener under load; the probes below
    /// send no bytes, so the fixture discards them and keeps waiting.
    ///
    /// A refused probe is retried instead of panicked on. Measured on this host:
    /// `connect` to an already-bound listener still returns `ConnectionRefused`
    /// while the accept loop is being scheduled, so an `expect` here failed the test
    /// for a condition the test is not about.
    async fn await_loopback_ready(address: std::net::SocketAddr) {
        for attempt in 0..40_u32 {
            if std::net::TcpStream::connect(address).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(u64::from(attempt.min(8)) * 10 + 10)).await;
        }
        panic!("fixture listener at {address} refused every readiness probe");
    }

    /// Accept connections until one sends a request head.
    ///
    /// A readiness probe connects and closes without sending anything, and it must
    /// not consume the single scripted response, so the loop accepts again. A reset
    /// is the same kind of probe: the client dropped the connection before sending a
    /// head. Failing on either was measured as `fixture reads: ...` on this host,
    /// which is a probe artifact rather than the behavior under test.
    ///
    /// The pattern nesting is real and cannot be unnested: `timeout(read)` yields
    /// `Result<Result<usize>>`, so the two `Err` levels mean different things and
    /// only this arm treats them alike.
    #[allow(
        clippy::unnested_or_patterns,
        reason = "timeout over a read is genuinely a nested Result"
    )]
    async fn accept_the_request(listener: TcpListener) -> tokio::net::TcpStream {
        loop {
            let (mut candidate, _) = listener.accept().await.expect("fixture accepts");
            let mut probe = [0_u8; 1];
            match tokio::time::timeout(Duration::from_millis(250), candidate.read(&mut probe)).await
            {
                Ok(Ok(count)) if count > 0 => return candidate,
                Err(_) | Ok(Err(_)) | Ok(Ok(_)) => {}
            }
        }
    }

    fn provider_request() -> ProviderRequest {
        ProviderRequest::new(
            RequestId::generate(),
            "deepseek-chat",
            vec![ProviderMessage::new(MessageRole::User, "sửa lỗi parser")],
        )
    }

    #[tokio::test]
    async fn g1_mock_stream_matches_the_buffered_boundary_event_for_event() {
        let script = vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("xin "),
            ProviderStreamEvent::text("chào"),
            ProviderStreamEvent::completed("stop"),
        ];
        let mock = MockProvider::scripted(script.clone());
        let streamed =
            collect_events(mock.stream_events(provider_request(), CancellationToken::new()))
                .await
                .expect("the stream collects");
        assert_eq!(streamed.len(), script.len());
        assert_eq!(streamed[1], ProviderStreamEvent::text("xin "));

        let buffered = mock
            .stream(provider_request(), CancellationToken::new())
            .await
            .expect("the buffered boundary still works");
        assert_eq!(
            buffered.len(),
            streamed.len(),
            "the additive boundary must not change the buffered one"
        );
        assert_eq!(mock.call_count(), 2);
    }

    #[tokio::test]
    async fn g1_cancellation_before_dispatch_reports_a_canceled_call() {
        let mock = MockProvider::scripted(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text("never sent"),
            ProviderStreamEvent::completed("stop"),
        ]);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = collect_events(mock.stream_events(provider_request(), cancellation))
            .await
            .expect_err("a canceled call must not look successful");
        assert!(error.to_string().contains("provider_canceled"), "{error}");
    }

    /// I10 core: text must be visible while the server is still holding the
    /// response open. A buffered provider cannot pass this test.
    #[tokio::test]
    async fn g1_adapter_delivers_text_before_the_response_completes() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        // Binding a port only puts the socket into listen; the accept loop below is
        // scheduled by the task. Signalling readiness from inside the task, and
        // skipping a connection that closes without sending a request head, removes
        // the window in which this environment refuses the first connection. That
        // refusal is the loopback flake the gate reports as
        // `provider_protocol ... error sending request for url`.
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let _ = ready_sender.send(());
            let mut socket = accept_the_request(listener).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("fixture writes the head");
            socket
                .write_all(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n",
                )
                .await
                .expect("fixture writes the first delta");
            socket.flush().await.expect("fixture flushes");
            // Barrier: the body stays open until the client has seen the delta.
            let _ = released.await;
            socket
                .write_all(
                    b"data: {\"choices\":[{\"delta\":{\"content\":\" second\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
                )
                .await
                .expect("fixture writes the rest");
            socket
                .shutdown()
                .await
                .expect("fixture half-closes response");
            // Keep the accepted socket alive until reqwest consumes the final SSE
            // bytes. Dropping both halves immediately after shutdown can surface as
            // an intermittent `error decoding response body` on Windows.
            let mut trailing = [0_u8; 256];
            let _ = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    match socket.read(&mut trailing).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await;
        });

        let adapter = DeepSeekAdapter::new(
            format!("http://{address}/chat/completions"),
            Arc::new(StaticCredentialResolver::new("fixture-secret")),
            ModelCapabilities::deepseek_fixture(),
        )
        .expect("adapter config");
        ready_receiver.await.expect("fixture task is scheduled");
        // The signal only proves the task is scheduled. Spaced probes confirm the
        // accept loop is running, and a bounded retry covers the moment this
        // environment refuses a fresh loopback connection under load. The fixture
        // treats a probe (connects, sends nothing, closes) as "not a request" and
        // keeps accepting, so neither measure can consume the scripted response.
        await_loopback_ready(address).await;
        let mut last = None;
        let mut stream = None;
        for attempt in 0..10 {
            let mut candidate = adapter.stream_events(provider_request(), CancellationToken::new());
            match tokio::time::timeout(Duration::from_millis(1500), candidate.next()).await {
                Ok(Some(Err(error))) if attempt < 5 => {
                    last = Some(error.to_string());
                    // Under load the refusal can persist for a few hundred ms.
                    tokio::time::sleep(Duration::from_millis(50 * (1 << attempt.min(6)))).await;
                }
                Ok(Some(Err(error))) => panic!("fixture stream failed: {error}"),
                Ok(Some(Ok(event))) => {
                    stream = Some((candidate, event));
                    break;
                }
                Ok(None) => panic!("fixture stream closed: {last:?}"),
                Err(_) => {
                    // No bytes yet: the call is open, which is what this test wants.
                    stream = Some((candidate, ProviderStreamEvent::started()));
                    break;
                }
            }
        }
        let (mut stream, first) = stream.expect("the fixture stream opens");

        let first = if first == ProviderStreamEvent::started() {
            tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .expect("a delta arrives before the barrier is released")
                .expect("the stream is open")
                .expect("the delta decodes")
        } else {
            first
        };
        assert_eq!(first, ProviderStreamEvent::text("first"));
        assert!(!release.is_closed(), "the barrier was still held");

        release.send(()).expect("the barrier releases");
        let rest = collect_events(stream).await.expect("the rest collects");
        assert!(
            rest.iter().any(ProviderStreamEvent::is_completed),
            "{rest:?}"
        );
        let text: String = rest
            .iter()
            .filter_map(|event| match event {
                ProviderStreamEvent::TextDelta { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, " second");
        server.await.expect("fixture server finishes");
    }
}
