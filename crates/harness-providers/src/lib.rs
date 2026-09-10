#![forbid(unsafe_code)]

//! Provider boundary for the P2 runtime.
//! Providers normalize transport only; execution and durable state remain in
//! the runtime layer.

use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt;
use harness_types::{ErrorCode, RequestId};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
pub use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub provider_id: String,
    pub model: String,
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub fixture: bool,
}

impl ModelCapabilities {
    #[must_use]
    pub fn deepseek_fixture() -> Self {
        Self {
            provider_id: "deepseek".to_owned(),
            model: "deepseek-chat".to_owned(),
            supports_streaming: true,
            supports_tools: true,
            fixture: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub role: MessageRole,
    pub content: String,
}

impl ProviderMessage {
    #[must_use]
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub request_id: RequestId,
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    #[serde(default)]
    pub tool_schemas: Vec<Value>,
    pub temperature: Option<f32>,
    pub metadata: Value,
}

impl ProviderRequest {
    #[must_use]
    pub fn new(
        request_id: RequestId,
        model: impl Into<String>,
        messages: Vec<ProviderMessage>,
    ) -> Self {
        Self {
            request_id,
            model: model.into(),
            messages,
            tool_schemas: Vec::new(),
            temperature: None,
            metadata: json!({}),
        }
    }

    #[must_use]
    pub fn with_tool_schemas(mut self, tool_schemas: Vec<Value>) -> Self {
        self.tool_schemas = tool_schemas;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderStreamEvent {
    Started {
        request_id: RequestId,
    },
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        call_id: String,
        name: String,
        arguments: String,
    },
    Completed {
        finish_reason: String,
    },
}

impl ProviderStreamEvent {
    #[must_use]
    pub fn started() -> Self {
        Self::Started {
            request_id: RequestId::generate(),
        }
    }
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::TextDelta { text: text.into() }
    }
    #[must_use]
    pub fn tool_delta(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self::ToolCallDelta {
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
    #[must_use]
    pub fn completed(reason: impl Into<String>) -> Self {
        Self::Completed {
            finish_reason: reason.into(),
        }
    }
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizedToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderResponse {
    pub text: String,
    pub finish_reason: Option<String>,
    pub incomplete_tool_calls: bool,
    pub tool_calls: Vec<NormalizedToolCall>,
}

pub fn assemble_stream(events: &[ProviderStreamEvent]) -> Result<ProviderResponse, ProviderError> {
    let mut text = String::new();
    let mut finish_reason = None;
    let mut calls: BTreeMap<String, NormalizedToolCall> = BTreeMap::new();
    for event in events {
        match event {
            ProviderStreamEvent::TextDelta { text: delta } => text.push_str(delta),
            ProviderStreamEvent::ToolCallDelta {
                call_id,
                name,
                arguments,
            } => {
                let entry = calls
                    .entry(call_id.clone())
                    .or_insert_with(|| NormalizedToolCall {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                    });
                if !name.is_empty() {
                    entry.name.clone_from(name);
                }
                entry.arguments.push_str(arguments);
            }
            ProviderStreamEvent::Completed {
                finish_reason: reason,
            } => finish_reason = Some(reason.clone()),
            ProviderStreamEvent::Started { .. } => {}
        }
    }
    let incomplete_tool_calls = calls
        .values()
        .any(|call| serde_json::from_str::<Value>(&call.arguments).is_err());
    Ok(ProviderResponse {
        text,
        finish_reason,
        incomplete_tool_calls,
        tool_calls: calls.into_values().collect(),
    })
}

#[derive(Clone, Debug, Error)]
#[error("{code}: {message}")]
pub struct ProviderError {
    code: ErrorCode,
    message: String,
}

impl ProviderError {
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

pub type ProviderFuture =
    Pin<Box<dyn Future<Output = Result<Vec<ProviderStreamEvent>, ProviderError>> + Send>>;

pub trait ModelProvider: Send + Sync {
    fn capabilities(&self) -> ModelCapabilities;
    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture;
}

#[derive(Clone)]
pub struct MockProvider {
    script: Arc<Vec<ProviderStreamEvent>>,
    delay_ms: u64,
    calls: Arc<AtomicUsize>,
    capabilities: ModelCapabilities,
}

impl MockProvider {
    #[must_use]
    pub fn scripted(script: Vec<ProviderStreamEvent>) -> Self {
        Self {
            script: Arc::new(script),
            delay_ms: 0,
            calls: Arc::new(AtomicUsize::new(0)),
            capabilities: ModelCapabilities::deepseek_fixture(),
        }
    }
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::scripted(vec![
            ProviderStreamEvent::started(),
            ProviderStreamEvent::text(text),
            ProviderStreamEvent::completed("stop"),
        ])
    }
    #[must_use]
    pub fn delayed_text(text: impl Into<String>, delay_ms: u64) -> Self {
        let mut provider = Self::text(text);
        provider.delay_ms = delay_ms;
        provider
    }
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl ModelProvider for MockProvider {
    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }
    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let script = Arc::clone(&self.script);
        let calls = Arc::clone(&self.calls);
        let delay_ms = self.delay_ms;
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            if cancellation.is_cancelled() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderCanceled,
                    "provider call canceled before dispatch",
                ));
            }
            if delay_ms > 0 {
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_millis(delay_ms)) => {},
                    () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider call canceled")),
                }
            }
            if cancellation.is_cancelled() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderCanceled,
                    "provider call canceled",
                ));
            }
            let mut result = (*script).clone();
            if let Some(ProviderStreamEvent::Started { request_id }) = result.first_mut() {
                *request_id = request.request_id;
            }
            Ok(result)
        })
    }
}

pub trait CredentialResolver: Send + Sync {
    fn resolve(&self) -> Result<String, ProviderError>;
}

#[derive(Clone)]
pub struct StaticCredentialResolver {
    secret: Arc<String>,
}

impl StaticCredentialResolver {
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(secret.into()),
        }
    }
}
impl CredentialResolver for StaticCredentialResolver {
    fn resolve(&self) -> Result<String, ProviderError> {
        Ok((*self.secret).clone())
    }
}

pub struct DeepSeekAdapter {
    endpoint: String,
    credentials: Arc<dyn CredentialResolver>,
    capabilities: ModelCapabilities,
    client: Client,
}

impl DeepSeekAdapter {
    pub fn new(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
    ) -> Result<Self, ProviderError> {
        let endpoint = endpoint.into();
        if endpoint.trim().is_empty() {
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                "provider endpoint is empty",
            ));
        }
        Ok(Self {
            endpoint,
            credentials,
            capabilities,
            client: Client::new(),
        })
    }
}

impl ModelProvider for DeepSeekAdapter {
    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }
    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let endpoint = self.endpoint.clone();
        let credentials = Arc::clone(&self.credentials);
        let client = self.client.clone();
        Box::pin(async move {
            let token = credentials.resolve()?;
            let mut body = json!({ "model": request.model, "messages": request.messages, "stream": true, "temperature": request.temperature });
            if !request.tool_schemas.is_empty()
                && let Some(object) = body.as_object_mut()
            {
                object.insert("tools".to_owned(), Value::Array(request.tool_schemas));
            }
            let response = tokio::select! {
                result = client.post(endpoint).bearer_auth(token).json(&body).send() => result.map_err(|error| ProviderError::new(ErrorCode::ProviderProtocol, format!("provider request failed: {error}")))?,
                () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider request canceled")),
            };
            if !response.status().is_success() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    format!("provider returned HTTP {}", response.status()),
                ));
            }
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::new();
            let mut events = Vec::new();
            while let Some(chunk) = tokio::select! { item = stream.next() => item, () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider stream canceled")), }
            {
                let chunk = chunk.map_err(|error| {
                    ProviderError::new(
                        ErrorCode::ProviderProtocol,
                        format!("provider stream failed: {error}"),
                    )
                })?;
                events.extend(decoder.feed(&chunk)?);
            }
            events.extend(decoder.finish()?);
            Ok(events)
        })
    }
}

#[derive(Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.buffer.extend_from_slice(bytes);
        self.drain_frames(false)
    }
    pub fn finish(&mut self) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.drain_frames(true)
    }
    fn drain_frames(
        &mut self,
        final_chunk: bool,
    ) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        let mut output = Vec::new();
        loop {
            let lf = self.buffer.windows(2).position(|window| window == b"\n\n");
            let crlf = self
                .buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n");
            let (index, width) = match (lf, crlf) {
                (Some(a), Some(b)) if b < a => (b, 4),
                (Some(a), _) => (a, 2),
                (_, Some(b)) => (b, 4),
                _ => break,
            };
            let frame = self.buffer.drain(..index + width).collect::<Vec<_>>();
            let text = String::from_utf8(frame).map_err(|_| {
                ProviderError::new(ErrorCode::ProviderProtocol, "SSE frame is not UTF-8")
            })?;
            let data = text
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                output.push(ProviderStreamEvent::completed("stop"));
            } else {
                output.push(parse_sse_payload(&data)?);
            }
        }
        if final_chunk && !self.buffer.iter().all(u8::is_ascii_whitespace) {
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                "unterminated SSE frame",
            ));
        }
        Ok(output)
    }
}

fn parse_sse_payload(data: &str) -> Result<ProviderStreamEvent, ProviderError> {
    let value: Value = serde_json::from_str(data).map_err(|error| {
        ProviderError::new(
            ErrorCode::ProviderProtocol,
            format!("malformed provider SSE JSON: {error}"),
        )
    })?;
    let choice = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| {
            ProviderError::new(
                ErrorCode::ProviderProtocol,
                "provider SSE frame has no choice",
            )
        })?;
    let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
    if let Some(content) = delta.get("content").and_then(Value::as_str) {
        return Ok(ProviderStreamEvent::text(content));
    }
    if let Some(tool) = delta
        .get("tool_calls")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
    {
        let function = tool.get("function").cloned().unwrap_or_else(|| json!({}));
        return Ok(ProviderStreamEvent::tool_delta(
            tool.get("id")
                .and_then(Value::as_str)
                .unwrap_or("tool-call"),
            function.get("name").and_then(Value::as_str).unwrap_or(""),
            function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ));
    }
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        return Ok(ProviderStreamEvent::completed(reason));
    }
    Ok(ProviderStreamEvent::text(""))
}
