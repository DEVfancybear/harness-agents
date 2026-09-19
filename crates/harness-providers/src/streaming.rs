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
    CancellationToken, DeepSeekAdapter, MockProvider, ModelCapabilities, ModelProvider,
    ProviderError, ProviderRequest, ProviderStreamEvent, SseDecoder,
};

/// Incremental event stream from one provider call.
pub type ProviderEventStream =
    Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send>>;

/// A provider that reports events while the response is still arriving.
pub trait StreamingModelProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;

    /// Start one call and return its event stream.
    ///
    /// Implementations must not wait for the response to finish: the first text
    /// delta is delivered as soon as it is decoded.
    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream;
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

impl StreamingModelProvider for MockProvider {
    fn capabilities(&self) -> ModelCapabilities {
        <Self as ModelProvider>::capabilities(self)
    }

    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        let script = Arc::clone(&self.script);
        let calls = Arc::clone(&self.calls);
        let delay_ms = self.delay_ms;
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

impl StreamingModelProvider for DeepSeekAdapter {
    fn capabilities(&self) -> ModelCapabilities {
        <Self as ModelProvider>::capabilities(self)
    }

    // The transport loop is deliberately linear: request, decode, forward. It is
    // long but has no branching business logic.
    #[allow(clippy::too_many_lines)]
    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        let endpoint = self.endpoint.clone();
        let credentials = Arc::clone(&self.credentials);
        let client = self.client.clone();
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
                "messages": request.messages,
                "stream": true,
                "temperature": request.temperature,
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
    use super::{StreamingModelProvider, collect_events};
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
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("fixture accepts");
            let mut request_bytes = vec![0_u8; 2048];
            let _ = socket
                .read(&mut request_bytes)
                .await
                .expect("fixture reads");
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
            socket.shutdown().await.ok();
        });

        let adapter = DeepSeekAdapter::new(
            format!("http://{address}/chat/completions"),
            Arc::new(StaticCredentialResolver::new("fixture-secret")),
            ModelCapabilities::deepseek_fixture(),
        )
        .expect("adapter config");
        let mut stream = adapter.stream_events(provider_request(), CancellationToken::new());

        let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("a delta arrives before the barrier is released")
            .expect("the stream is open")
            .expect("the delta decodes");
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
