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

mod streaming;
pub use streaming::{ProviderEventStream, collect_events};

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

    /// The message as OpenAI-compatible chat APIs accept it.
    ///
    /// The one shape this app can send without carrying tool-call metadata is a
    /// plain `system`/`user`/`assistant` message. A tool result is therefore sent
    /// as a **user** message that names the tool, which is not a stylistic choice:
    ///
    /// - the `tool` role requires a `tool_call_id`, and the assistant message before
    ///   it must carry the matching `tool_calls`. Measured against the live API, a
    ///   `tool` message without one is refused with
    ///   `HTTP 422 ... missing field 'tool_call_id'`;
    /// - the Chat Completions API does not support inserting tool calls
    ///   mid-conversation at all, so replaying the structured pair is not an option
    ///   this endpoint offers (the Anthropic and Responses APIs are the ones that
    ///   do);
    /// - the result text is what the model actually needs, and this keeps it in the
    ///   conversation with every provider that speaks this format.
    ///
    /// The marker is explicit so a tool result can never read as something the user
    /// said.
    #[must_use]
    pub fn to_wire(&self) -> Value {
        match self.role {
            MessageRole::Tool => json!({
                "role": "user",
                "content": format!("[tool result]\n{}", self.content),
            }),
            MessageRole::System => json!({ "role": "system", "content": self.content }),
            MessageRole::User => json!({ "role": "user", "content": self.content }),
            MessageRole::Assistant => json!({ "role": "assistant", "content": self.content }),
        }
    }
}

/// The `messages` array as the API accepts it.
#[must_use]
pub fn wire_messages(messages: &[ProviderMessage]) -> Vec<Value> {
    messages.iter().map(ProviderMessage::to_wire).collect()
}

/// Thinking mode is off, deliberately.
///
/// The provider enables thinking mode by default, and a request that turns it on
/// must send every assistant `reasoning_content` back on the next request —
/// measured as `HTTP 400 The 'reasoning_content' in the thinking mode must be
/// passed back to the API`. This app does not retain reasoning content, so it asks
/// for the mode it can actually complete instead of failing on the second turn.
#[must_use]
pub fn thinking_disabled() -> Value {
    json!({ "type": "disabled" })
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
    // A call with no name or with arguments that never completed JSON cannot be
    // executed; the flag is what lets a caller refuse the whole response instead of
    // reporting one confusing tool failure per fragment.
    let incomplete_tool_calls = calls.values().any(|call| {
        call.name.trim().is_empty() || serde_json::from_str::<Value>(&call.arguments).is_err()
    });
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

    /// Incremental view of one call.
    ///
    /// The default forwards the buffered result once, so an implementor that can
    /// only answer in one piece still works. Providers that decode a transport
    /// stream override this and deliver each event as it arrives.
    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        streaming::bridge_buffered(self.stream(request, cancellation))
    }
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

    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        streaming::mock_stream(self, request, cancellation)
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

/// The path a chat completion is posted to when the caller named only a base URL.
pub const CHAT_COMPLETIONS_PATH: &str = "/chat/completions";

/// Make an endpoint name the chat-completions resource, whatever form it arrived in.
///
/// A base URL such as `https://api.deepseek.com` is what the provider documents and
/// what an operator naturally configures, but posting to the bare host asks for `/`
/// and the API answers **404** — which reads like a wrong model or a wrong key when
/// it is neither. This turns every accepted spelling into the one resource path:
///
/// - `https://api.deepseek.com` and a trailing slash both gain `/chat/completions`;
/// - `https://api.deepseek.com/v1` gains it after the version, so the OpenAI-style
///   base URL works too;
/// - an endpoint that already names the resource keeps the path it was given, and
///   only a trailing slash is trimmed.
///
/// This happens once, when the adapter is built, so both call paths post to the
/// same string.
#[must_use]
pub fn chat_completions_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return trimmed.to_owned();
    }
    // The scheme's own `//` is not a path segment, so the path starts after it.
    let path_start = trimmed.find("://").map_or(0, |scheme| scheme + "://".len());
    let has_path = trimmed[path_start..].contains('/');
    if !has_path {
        return format!("{trimmed}{CHAT_COMPLETIONS_PATH}");
    }
    if trimmed.ends_with(CHAT_COMPLETIONS_PATH) {
        return trimmed.to_owned();
    }
    if trimmed.ends_with("/v1") {
        return format!("{trimmed}{CHAT_COMPLETIONS_PATH}");
    }
    trimmed.to_owned()
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
        let endpoint = chat_completions_endpoint(&endpoint.into());
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
    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> ProviderEventStream {
        streaming::adapter_stream(self, request, cancellation)
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let endpoint = self.endpoint.clone();
        let credentials = Arc::clone(&self.credentials);
        let client = self.client.clone();
        Box::pin(async move {
            let token = credentials.resolve()?;
            let mut body = json!({ "model": request.model, "messages": wire_messages(&request.messages), "stream": true, "temperature": request.temperature, "thinking": thinking_disabled() });
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

/// Identity a fragment gets when the stream never announced one.
const DEFAULT_TOOL_CALL_ID: &str = "tool-call";

#[derive(Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    /// Call identity announced for each streamed tool-call `index`.
    ///
    /// The fragment that opens a call carries `id` and `function.name`; every later
    /// fragment of the same call carries `index` and `function.arguments` only, so
    /// the index is the only field that identifies the call for the whole stream.
    tool_call_ids: BTreeMap<u64, String>,
    /// Call the most recent fragment belonged to, for a stream that omits `index`.
    last_tool_call_id: Option<String>,
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
                output.extend(self.frame_events(&data)?);
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

impl SseDecoder {
    /// Every event one SSE `data:` payload carries, in wire order.
    ///
    /// A frame can carry prose and tool-call fragments together, so nothing is
    /// dropped in favour of the first match. A frame with neither is the terminal
    /// one when it names a reason, and an empty text delta otherwise, which is what
    /// the P2 boundary already reported for such a frame.
    fn frame_events(&mut self, data: &str) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
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
        let mut events = Vec::new();
        if let Some(content) = delta.get("content").and_then(Value::as_str) {
            events.push(ProviderStreamEvent::text(content));
        }
        if let Some(fragments) = delta.get("tool_calls").and_then(Value::as_array) {
            for fragment in fragments {
                events.push(self.tool_call_fragment(fragment));
            }
        }
        // A terminal frame can carry the last of the answer with it, so the reason is
        // reported in addition to that content: taking whichever came first dropped
        // `finish_reason` from every frame that also carried prose or a fragment. A
        // frame with neither stays what it always was — the terminal event when it
        // names a reason, an empty text delta otherwise.
        match choice.get("finish_reason").and_then(Value::as_str) {
            Some(reason) => events.push(ProviderStreamEvent::completed(reason)),
            None if events.is_empty() => events.push(ProviderStreamEvent::text("")),
            None => {}
        }
        Ok(events)
    }

    /// One streamed tool-call fragment, stamped with the identity of its call.
    ///
    /// The measured `DeepSeek` stream (OpenAI-compatible) opens a call with a frame
    /// that carries `index`, `id` and `function.name`, and then sends one frame per
    /// argument fragment carrying `index` and `function.arguments` only. Reading the
    /// id off each frame therefore turned one call into two — a named call with no
    /// arguments and an anonymous call holding them — and both were rejected by the
    /// execution gate, at 0 ms, as `provider_protocol` and `policy_denied`. `index`
    /// is present for the whole call, so the announced id is remembered per index
    /// and stamped onto every later fragment.
    fn tool_call_fragment(&mut self, fragment: &Value) -> ProviderStreamEvent {
        let announced = fragment
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let index = fragment.get("index").and_then(Value::as_u64);
        let call_id = match (index, announced) {
            (Some(index), Some(id)) => {
                self.tool_call_ids.insert(index, id.to_owned());
                id.to_owned()
            }
            // A continuation fragment: only the index says which call it belongs to.
            (Some(index), None) => self
                .tool_call_ids
                .entry(index)
                .or_insert_with(|| format!("{DEFAULT_TOOL_CALL_ID}-{index}"))
                .clone(),
            (None, Some(id)) => id.to_owned(),
            // Neither field: the fragment continues the call announced most
            // recently, and only a stream that never announced one keeps the
            // historical placeholder, which the gate then reports as malformed.
            (None, None) => self
                .last_tool_call_id
                .clone()
                .unwrap_or_else(|| DEFAULT_TOOL_CALL_ID.to_owned()),
        };
        self.last_tool_call_id = Some(call_id.clone());
        let function = fragment
            .get("function")
            .cloned()
            .unwrap_or_else(|| json!({}));
        ProviderStreamEvent::tool_delta(
            call_id,
            function.get("name").and_then(Value::as_str).unwrap_or(""),
            function
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or(""),
        )
    }
}

#[cfg(test)]
mod sse_tool_call_tests {
    use super::{ProviderStreamEvent, SseDecoder, assemble_stream};
    use serde_json::json;

    /// The measured wire shape of one call: this frame opens it, and the argument
    /// fragments that follow carry `index` and `arguments` only.
    const OPENING_FRAME: &str = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":null,\"tool_calls\":[{\"index\":0,",
        "\"id\":\"call_00_aDELHVxEkgZ5FfVqhk7K7693\",\"type\":\"function\",",
        "\"function\":{\"name\":\"list_files\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n"
    );

    /// One continuation fragment, spelled the way the provider spelled it.
    fn argument_frame(arguments: &str) -> String {
        let payload = json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 0, "function": {"arguments": arguments}}]},
                "finish_reason": null,
            }],
        });
        format!("data: {payload}\n\n")
    }

    fn tool_events(events: &[ProviderStreamEvent]) -> Vec<(&str, &str, &str)> {
        events
            .iter()
            .filter_map(|event| match event {
                ProviderStreamEvent::ToolCallDelta {
                    call_id,
                    name,
                    arguments,
                } => Some((call_id.as_str(), name.as_str(), arguments.as_str())),
                _ => None,
            })
            .collect()
    }

    /// Regression: one call fragmented over ten frames stayed one call.
    ///
    /// Reading the id off each frame made two calls out of this — `list_files` with
    /// no arguments and an anonymous call holding `{"path": "."}` — and the gate
    /// rejected both at 0 ms, which is the failure a user saw as two red tool cards.
    /// The bytes are also fed in chunks that cut frames apart, because a real socket
    /// does not deliver frames.
    #[test]
    fn sse_fragments_of_one_call_keep_one_identity() {
        let mut wire = String::from(OPENING_FRAME);
        for fragment in ["{", "\"", "path", "\"", ": ", "\"", ".", "\"", "}"] {
            wire.push_str(&argument_frame(fragment));
        }
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        for chunk in wire.as_bytes().chunks(7) {
            events.extend(decoder.feed(chunk).expect("frames decode"));
        }
        events.extend(decoder.finish().expect("the stream ends"));

        let fragments = tool_events(&events);
        assert_eq!(fragments.len(), 10, "one fragment per frame: {events:?}");
        assert_eq!(
            fragments[0].1, "list_files",
            "the opening frame names the call"
        );
        for (call_id, _, _) in &fragments {
            assert_eq!(
                *call_id, "call_00_aDELHVxEkgZ5FfVqhk7K7693",
                "every fragment must belong to the announced call: {events:?}"
            );
        }

        let response = assemble_stream(&events).expect("normalized response");
        assert_eq!(
            response.tool_calls.len(),
            1,
            "one call, not one per fragment: {:?}",
            response.tool_calls
        );
        assert_eq!(response.tool_calls[0].name, "list_files");
        assert_eq!(response.tool_calls[0].arguments, "{\"path\": \".\"}");
        assert!(!response.incomplete_tool_calls);
    }

    /// A frame that carries the end of the answer still reports why it ended.
    ///
    /// Taking the first match dropped `finish_reason` whenever the terminal frame also
    /// carried prose or a tool fragment, so the normalized response lost the provider's
    /// own statement about how the turn finished.
    #[test]
    fn sse_a_finish_reason_survives_the_content_that_rides_with_it() {
        let mut decoder = SseDecoder::new();
        let prose = "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n";
        let events = decoder.feed(prose.as_bytes()).expect("the frame decodes");
        assert_eq!(
            events,
            vec![
                ProviderStreamEvent::text("done"),
                ProviderStreamEvent::completed("stop")
            ],
            "{events:?}"
        );

        let fragment = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_e\",",
            "\"function\":{\"name\":\"git_status\",\"arguments\":\"{}\"}}]},",
            "\"finish_reason\":\"tool_calls\"}]}\n\n"
        );
        let mut decoder = SseDecoder::new();
        let events = decoder
            .feed(fragment.as_bytes())
            .expect("the frame decodes");
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[1], ProviderStreamEvent::completed("tool_calls"));

        // The shape that carries only the reason keeps producing only the terminal event.
        let mut decoder = SseDecoder::new();
        let terminal = decoder
            .feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n")
            .expect("the frame decodes");
        assert_eq!(terminal, vec![ProviderStreamEvent::completed("stop")]);
    }

    /// Two calls opened in one frame stay separate, and a continuation finds its own.
    #[test]
    fn sse_parallel_calls_in_one_frame_stay_separate() {
        let opening = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_a\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}},",
            "{\"index\":1,\"id\":\"call_b\",\"function\":{\"name\":\"search_text\",\"arguments\":\"\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        );
        let continuation = json!({
            "choices": [{
                "delta": {"tool_calls": [{"index": 1, "function": {"arguments": "{\"query\":\"x\"}"}}]},
                "finish_reason": null,
            }],
        });
        let mut decoder = SseDecoder::new();
        let mut events = decoder.feed(opening.as_bytes()).expect("the opening frame");
        events.extend(
            decoder
                .feed(format!("data: {continuation}\n\n").as_bytes())
                .expect("the continuation frame"),
        );

        let response = assemble_stream(&events).expect("normalized response");
        assert_eq!(response.tool_calls.len(), 2, "{:?}", response.tool_calls);
        assert_eq!(response.tool_calls[0].call_id, "call_a");
        assert_eq!(response.tool_calls[0].name, "read_file");
        assert_eq!(response.tool_calls[0].arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(response.tool_calls[1].call_id, "call_b");
        assert_eq!(response.tool_calls[1].name, "search_text");
        assert_eq!(response.tool_calls[1].arguments, "{\"query\":\"x\"}");
        assert!(!response.incomplete_tool_calls);
    }

    /// A frame that carries prose and a fragment keeps both.
    #[test]
    fn sse_prose_and_a_fragment_in_one_frame_are_both_kept() {
        let frame = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"checking\",\"tool_calls\":[",
            "{\"index\":0,\"id\":\"call_c\",\"function\":{\"name\":\"git_status\",\"arguments\":\"{}\"}}",
            "]},\"finish_reason\":null}]}\n\n"
        );
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(frame.as_bytes()).expect("the frame decodes");
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[0], ProviderStreamEvent::text("checking"));
        assert_eq!(
            events[1],
            ProviderStreamEvent::tool_delta("call_c", "git_status", "{}")
        );
    }

    /// An adapter that omits `index` still keeps one call: the id opens it and the
    /// fragments after it continue the call that was announced most recently.
    #[test]
    fn sse_fragments_without_an_index_continue_the_announced_call() {
        let opening = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_d\",",
            "\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n"
        );
        let rest = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"function\":{\"arguments\":\"\\\"a.rs\\\"}\"}}]},\"finish_reason\":null}]}\n\n"
        );
        let mut decoder = SseDecoder::new();
        let mut events = decoder.feed(opening.as_bytes()).expect("the opening frame");
        events.extend(decoder.feed(rest.as_bytes()).expect("the rest"));

        let response = assemble_stream(&events).expect("normalized response");
        assert_eq!(response.tool_calls.len(), 1, "{:?}", response.tool_calls);
        assert_eq!(response.tool_calls[0].call_id, "call_d");
        assert_eq!(response.tool_calls[0].arguments, "{\"path\":\"a.rs\"}");
        assert!(!response.incomplete_tool_calls);
    }

    /// A fragment that announces nothing is still reported as a malformed call,
    /// so a stream this decoder cannot identify never looks executable.
    #[test]
    fn sse_an_anonymous_fragment_is_reported_as_incomplete() {
        let frame = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
            "{\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n"
        );
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(frame.as_bytes()).expect("the frame decodes");
        assert_eq!(
            tool_events(&events),
            vec![("tool-call", "", "{}")],
            "{events:?}"
        );
        let response = assemble_stream(&events).expect("normalized response");
        assert!(
            response.incomplete_tool_calls,
            "a call with no name cannot be executed: {:?}",
            response.tool_calls
        );
    }
}

#[cfg(test)]
mod endpoint_tests {
    use super::{CHAT_COMPLETIONS_PATH, chat_completions_endpoint};

    /// The measured bug: a bare base URL posted to `/` and the API answered 404.
    ///
    /// Every spelling below is one an operator can reasonably configure, and all of
    /// them have to name the same resource.
    #[test]
    fn a_base_url_gains_the_chat_completions_path() {
        assert_eq!(
            chat_completions_endpoint("https://api.deepseek.com"),
            format!("https://api.deepseek.com{CHAT_COMPLETIONS_PATH}")
        );
        assert_eq!(
            chat_completions_endpoint("https://api.deepseek.com/"),
            format!("https://api.deepseek.com{CHAT_COMPLETIONS_PATH}")
        );
        assert_eq!(
            chat_completions_endpoint("  https://api.deepseek.com  "),
            format!("https://api.deepseek.com{CHAT_COMPLETIONS_PATH}")
        );
        // The OpenAI-style base URL, with the version segment kept.
        assert_eq!(
            chat_completions_endpoint("https://api.deepseek.com/v1"),
            format!("https://api.deepseek.com/v1{CHAT_COMPLETIONS_PATH}")
        );
        // A bare host with no scheme is still a host, not a path.
        assert_eq!(
            chat_completions_endpoint("api.deepseek.com"),
            format!("api.deepseek.com{CHAT_COMPLETIONS_PATH}")
        );
    }

    #[test]
    fn an_endpoint_that_already_names_the_resource_is_left_alone() {
        for endpoint in [
            "https://api.deepseek.com/chat/completions",
            "https://api.deepseek.com/chat/completions/",
            "http://127.0.0.1:9/chat/completions",
        ] {
            assert_eq!(
                chat_completions_endpoint(endpoint),
                endpoint.trim_end_matches('/'),
                "{endpoint} must keep its own path"
            );
        }
    }

    /// A self-hosted path this app does not know is never guessed at.
    #[test]
    fn another_resource_path_is_not_replaced() {
        assert_eq!(
            chat_completions_endpoint("https://gateway.internal/openai/chat"),
            "https://gateway.internal/openai/chat"
        );
        assert_eq!(chat_completions_endpoint(""), "");
    }
}

#[cfg(test)]
mod wire_tests {
    use super::{MessageRole, ProviderMessage, thinking_disabled, wire_messages};

    /// The measured 422: a `tool` role message without `tool_call_id`.
    ///
    /// The wire shape this app sends therefore never uses the `tool` role. The
    /// result still reaches the model, marked so it cannot read as user text.
    #[test]
    fn a_tool_result_is_sent_as_a_marked_user_message() {
        let messages = vec![
            ProviderMessage::new(MessageRole::System, "be brief"),
            ProviderMessage::new(MessageRole::User, "list the files"),
            ProviderMessage::new(MessageRole::Assistant, "calling a tool"),
            ProviderMessage::new(MessageRole::Tool, "tool list_files failed: denied"),
        ];
        let wire = wire_messages(&messages);
        assert_eq!(wire.len(), 4);
        assert_eq!(wire[0]["role"], "system");
        assert_eq!(wire[1]["role"], "user");
        assert_eq!(wire[2]["role"], "assistant");
        assert_eq!(
            wire[3]["role"], "user",
            "the tool role is not sent: it needs tool_call_id and a matching tool_calls"
        );
        let content = wire[3]["content"].as_str().expect("content is a string");
        assert!(
            content.starts_with("[tool result]"),
            "a tool result must be marked: {content}"
        );
        assert!(content.contains("denied"), "{content}");
        for message in &wire {
            assert!(
                message.get("tool_call_id").is_none(),
                "no message may claim a tool_call_id this app does not have: {message}"
            );
        }
    }

    /// Measured 400: thinking mode demands `reasoning_content` back on turn two.
    #[test]
    fn thinking_mode_is_disabled_explicitly() {
        assert_eq!(
            thinking_disabled(),
            serde_json::json!({ "type": "disabled" })
        );
    }
}
