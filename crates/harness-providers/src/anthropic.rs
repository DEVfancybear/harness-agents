//! Anthropic Messages transport adapter using the shared provider contract.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures_util::StreamExt;
use harness_types::{ErrorCode, RequestId};
use reqwest::Client;
use serde_json::{Value, json};

use crate::{
    CancellationToken, CredentialResolver, ModelCapabilities, ModelProvider, ProviderError,
    ProviderFuture, ProviderRequest, ProviderStreamEvent,
};

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_TOKENS: u64 = 8192;
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

pub struct AnthropicMessagesAdapter {
    endpoint: String,
    credentials: Arc<dyn CredentialResolver>,
    capabilities: ModelCapabilities,
    client: Client,
    thinking: Option<crate::Thinking>,
    headers: Vec<(String, String)>,
}

impl AnthropicMessagesAdapter {
    /// Send the session's thinking level with every request.
    #[must_use]
    pub fn with_thinking(mut self, thinking: Option<crate::Thinking>) -> Self {
        self.thinking = thinking;
        self
    }

    /// Send these headers with every request, such as `OpenCode`'s session header.
    #[must_use]
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    pub fn new(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
    ) -> Result<Self, ProviderError> {
        let endpoint = endpoint.into();
        super::validate_endpoint(&endpoint)?;
        let client = super::shared_client(
            Duration::from_secs(super::DEFAULT_CONNECT_TIMEOUT_SECONDS),
            Duration::from_secs(super::DEFAULT_REQUEST_TIMEOUT_SECONDS),
        )
        .map_err(|_| {
            ProviderError::new(
                ErrorCode::ProviderProtocol,
                "provider client is not buildable",
            )
        })?;
        Ok(Self {
            endpoint,
            credentials,
            capabilities,
            client,
            thinking: None,
            headers: Vec::new(),
        })
    }

    #[allow(clippy::too_many_lines)] // the Messages API body, field by field
    fn request_body(request: &ProviderRequest, thinking: Option<&crate::Thinking>) -> Value {
        let mut system = Vec::new();
        let mut messages = Vec::new();
        for message in request.messages.iter() {
            match message.role {
                super::MessageRole::System => system.push(message.content.clone()),
                super::MessageRole::Tool => {
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id(message.tool_call_id.as_deref().unwrap_or("missing-call-id")),
                        "content": content_with_images(&message.content, &message.attachments)
                    });
                    // The results of one batch of calls go back as one user message,
                    // as the API documents: one block position for the cache's
                    // lookback instead of one per call.
                    let previous_holds_results = messages.last().is_some_and(|last: &Value| {
                        last["role"] == "user"
                            && last["content"].as_array().is_some_and(|blocks| {
                                blocks.iter().all(|block| block["type"] == "tool_result")
                            })
                    });
                    if previous_holds_results
                        && let Some(blocks) = messages
                            .last_mut()
                            .and_then(|last| last["content"].as_array_mut())
                    {
                        blocks.push(block);
                    } else {
                        messages.push(json!({"role": "user", "content": [block]}));
                    }
                }
                super::MessageRole::User => messages.push(json!({
                    "role": "user",
                    "content": content_with_images(&message.content, &message.attachments)
                })),
                super::MessageRole::Assistant => {
                    let mut blocks = Vec::new();
                    // A signed thinking block goes back first, as the API requires when
                    // thinking is on and the turn continues after a tool call.
                    // Only Anthropic's own signature goes back: a turn answered by
                    // another provider (a backup model) carries an OpenAI
                    // reasoning item there, which Anthropic would reject.
                    if thinking.is_some()
                        && let Some(reasoning) = &message.reasoning
                        && let Some(signature) = &reasoning.signature
                        && !signature.trim_start().starts_with('{')
                    {
                        blocks.push(json!({
                            "type": "thinking",
                            "thinking": reasoning.text,
                            "signature": signature,
                        }));
                    }
                    if !message.content.is_empty() {
                        blocks.push(json!({"type":"text", "text":message.content}));
                    }
                    for call in &message.tool_calls {
                        let input = serde_json::from_str::<Value>(&call.arguments)
                            .unwrap_or_else(|_| json!({}));
                        blocks.push(json!({
                            "type":"tool_use",
                            "id":tool_use_id(&call.call_id),
                            "name":call.name,
                            "input":input
                        }));
                    }
                    if blocks.is_empty() {
                        blocks.push(json!({"type":"text", "text":""}));
                    }
                    messages.push(json!({"role":"assistant", "content":blocks}));
                }
            }
        }
        let mut body = json!({
            "model": request.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "messages": messages,
            "stream": true
        });
        if let Some(max_tokens) = request.max_output_tokens {
            body["max_tokens"] = json!(max_tokens);
        }
        // prime-agent's prompt caching: breakpoints on the system prompt, the
        // last tool and the last user message, so the next request of the
        // conversation reads its prefix from the cache instead of paying for it
        // again. Without them Anthropic caches nothing.
        let cache_control = cache_control();
        if !system.is_empty() {
            body["system"] = json!([{
                "type": "text",
                "text": system.join("\n\n"),
                "cache_control": cache_control,
            }]);
        }
        let mut tools = request
            .tool_schemas
            .iter()
            .filter_map(anthropic_tool_schema)
            .collect::<Vec<_>>();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = cache_control.clone();
        }
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        // The moving breakpoint goes on the last user message. The fourth goes on
        // the user message before it - where the previous request of this
        // conversation put its own - so that entry is read by an exact match
        // however many blocks the step added: the API only looks 20 blocks back
        // from a breakpoint, and a long batch of calls can pass that.
        if let Some(messages) = body["messages"].as_array_mut() {
            let users = messages
                .iter()
                .enumerate()
                .filter(|(_, message)| message["role"] == "user")
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let last_is_user = messages.last().is_some_and(|last| last["role"] == "user");
            if last_is_user {
                for index in users.iter().rev().take(2) {
                    mark_cache_breakpoint(&mut messages[*index], &cache_control);
                }
            }
        }
        if let Some(thinking) = thinking {
            thinking.apply_anthropic(&mut body, &request.model);
        }
        body
    }
}

const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";

/// prime-agent's `normalizeToolCallId` for Anthropic: a call id another
/// provider made (Kimi's `functions.read:0`, a Responses `call_...|fc_...`)
/// becomes one Anthropic accepts - letters, digits, `_` and `-`, at most 64.
fn tool_use_id(id: &str) -> String {
    let normalized = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect::<String>();
    if normalized.is_empty() {
        "call".to_owned()
    } else {
        normalized
    }
}

/// Put `cache_control` on the last block of a user message that can carry one.
fn mark_cache_breakpoint(message: &mut Value, cache_control: &Value) {
    if let Some(text) = message["content"].as_str().map(str::to_owned) {
        message["content"] = json!([{
            "type": "text",
            "text": text,
            "cache_control": cache_control,
        }]);
    } else if let Some(block) = message["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
        .filter(|block| {
            matches!(
                block["type"].as_str(),
                Some("text" | "image" | "tool_result")
            )
        })
    {
        block["cache_control"] = cache_control.clone();
    }
}

/// prime-agent's `cache_control`: an ephemeral breakpoint, five minutes by
/// default, an hour with `HA_CACHE_RETENTION=long`.
fn cache_control() -> Value {
    if crate::cache_retention_long() {
        json!({"type": "ephemeral", "ttl": "1h"})
    } else {
        json!({"type": "ephemeral"})
    }
}

/// Text alone as a string; text with images as the block list the API reads images
/// from.
fn content_with_images(text: &str, images: &[crate::ImageAttachment]) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut blocks = vec![json!({"type": "text", "text": text})];
    for image in images {
        blocks.push(match &image.source {
            crate::ImageSource::Inline {
                media_type,
                data_base64,
            } => json!({
                "type": "image",
                "source": {"type": "base64", "media_type": media_type, "data": data_base64}
            }),
            crate::ImageSource::Remote { url } => json!({
                "type": "image",
                "source": {"type": "url", "url": url}
            }),
        });
    }
    Value::Array(blocks)
}

fn anthropic_tool_schema(schema: &Value) -> Option<Value> {
    let function = schema.get("function").unwrap_or(schema);
    let name = function.get("name")?.as_str()?;
    let mut tool = json!({
        "name": name,
        "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object"}))
    });
    if let Some(description) = function.get("description") {
        tool["description"] = description.clone();
    }
    Some(tool)
}

impl ModelProvider for AnthropicMessagesAdapter {
    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let events = self.stream_events(request, cancellation);
        Box::pin(crate::collect_events(events))
    }

    /// Each decoded event is handed on as its SSE frame arrives, as prime-agent
    /// streams an Anthropic answer; the whole response used to be collected
    /// first, so nothing showed until it ended.
    fn stream_events(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
    ) -> crate::ProviderEventStream {
        let (sender, mut receiver) =
            tokio::sync::mpsc::channel::<Result<ProviderStreamEvent, ProviderError>>(64);
        let call = self.call(request, cancellation, sender.clone());
        tokio::spawn(async move {
            if let Err(error) = call.await {
                let _ = sender.send(Err(error)).await;
            }
        });
        Box::pin(futures_util::stream::poll_fn(move |context| {
            receiver.poll_recv(context)
        }))
    }
}

impl AnthropicMessagesAdapter {
    /// One request: decoded events go out through `sender` frame by frame; a
    /// failure is returned for the caller to send last.
    #[allow(clippy::too_many_lines)] // one request, from the body to the last frame
    fn call(
        &self,
        request: ProviderRequest,
        cancellation: CancellationToken,
        sender: tokio::sync::mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    ) -> impl std::future::Future<Output = Result<(), ProviderError>> + Send + 'static {
        let endpoint = self.endpoint.clone();
        let target = crate::endpoint_target(&endpoint);
        let client = self.client.clone();
        let credentials = Arc::clone(&self.credentials);
        let thinking = self.thinking;
        let headers = self.headers.clone();
        let provider_id = self.capabilities.provider_id.clone();
        // prime-agent's beta features: tool input streamed as it is written
        // when the request has tools, and thinking between tool calls on the
        // models without adaptive thinking.
        let mut betas = Vec::new();
        if !request.tool_schemas.is_empty() {
            betas.push(FINE_GRAINED_TOOL_STREAMING_BETA);
        }
        if thinking
            .as_ref()
            .is_some_and(|thinking| thinking.level != crate::ThinkingLevel::Off)
            && !crate::thinking::supports_adaptive_thinking(&request.model.to_ascii_lowercase())
        {
            betas.push(INTERLEAVED_THINKING_BETA);
        }
        let beta = betas.join(",");
        async move {
            let token = credentials.resolve()?;
            let post = headers
                .iter()
                .fold(client.post(endpoint), |post, (name, value)| {
                    post.header(name, value)
                });
            let post = if beta.is_empty() {
                post
            } else {
                post.header("anthropic-beta", beta)
            };
            let response = tokio::select! {
                result = post
                    .header("x-api-key", token)
                    .header("anthropic-version", ANTHROPIC_VERSION)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .json(&Self::request_body(&request, thinking.as_ref()))
                    .send() => result.map_err(|error| {
                        let error = error.without_url();
                        ProviderError::new(
                            if error.is_timeout() { ErrorCode::ProcessTimedOut } else { ErrorCode::ServiceUnavailable },
                            format!("provider request failed: {error}{target}"),
                        )
                    })?,
                () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider request canceled")),
            };
            super::limits::record(&provider_id, response.headers());
            if !response.status().is_success() {
                return Err(super::http_response_error(response).await);
            }
            let mut stream = response.bytes_stream();
            let mut decoder = AnthropicSseDecoder::default();
            let mut events = vec![ProviderStreamEvent::Started {
                request_id: request.request_id.clone(),
            }];
            // Hand on what is decoded; `false` once the reader went away. One
            // `Started` opens the call: the decoder adds one at the end when it
            // sees none in what is left, and a second would read as a retry.
            let mut started = false;
            let mut flush = |events: &mut Vec<ProviderStreamEvent>| {
                let sender = sender.clone();
                let batch = std::mem::take(events)
                    .into_iter()
                    .filter(|event| {
                        let first =
                            !matches!(event, ProviderStreamEvent::Started { .. }) || !started;
                        if matches!(event, ProviderStreamEvent::Started { .. }) {
                            started = true;
                        }
                        first
                    })
                    .collect::<Vec<_>>();
                async move {
                    for event in batch {
                        if sender.send(Ok(event)).await.is_err() {
                            return false;
                        }
                    }
                    true
                }
            };
            if !flush(&mut events).await {
                return Ok(());
            }
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider stream canceled")),
                };
                let Some(item) = item else {
                    break;
                };
                let chunk = item.map_err(|error| {
                    ProviderError::new(
                        ErrorCode::ProviderProtocol,
                        format!("provider stream failed: {}", error.without_url()),
                    )
                })?;
                decoder.feed(&chunk, &mut events, &request.request_id)?;
                if !flush(&mut events).await {
                    return Ok(());
                }
            }
            decoder.finish(&mut events, &request.request_id)?;
            flush(&mut events).await;
            Ok(())
        }
    }
}

#[derive(Default)]
struct AnthropicSseDecoder {
    buffer: Vec<u8>,
    total_bytes: usize,
    saw_message_stop: bool,
    tools: BTreeMap<u64, (String, String)>,
    prompt_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    output_tokens: u64,
}

impl AnthropicSseDecoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        events: &mut Vec<ProviderStreamEvent>,
        request_id: &RequestId,
    ) -> Result<(), ProviderError> {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        if self.total_bytes > MAX_STREAM_BYTES {
            return Err(protocol("provider stream exceeds the size limit"));
        }
        self.buffer.extend_from_slice(bytes);
        while let Some(end) = self.buffer.windows(2).position(|pair| pair == b"\n\n") {
            let frame = self.buffer.drain(..end + 2).collect::<Vec<_>>();
            self.handle_frame(&frame, events, request_id)?;
        }
        Ok(())
    }

    fn finish(
        &mut self,
        events: &mut Vec<ProviderStreamEvent>,
        request_id: &RequestId,
    ) -> Result<(), ProviderError> {
        if !self.buffer.is_empty() {
            return Err(protocol("provider stream ended in a partial SSE frame"));
        }
        if !self.saw_message_stop {
            return Err(protocol("provider stream ended before message_stop"));
        }
        events.push(
            ProviderStreamEvent::usage(
                self.prompt_tokens,
                self.output_tokens,
                self.prompt_tokens.saturating_add(self.output_tokens),
            )
            .with_cache(self.cache_read_tokens, self.cache_write_tokens),
        );
        if !events
            .iter()
            .any(|event| matches!(event, ProviderStreamEvent::Started { .. }))
        {
            events.insert(
                0,
                ProviderStreamEvent::Started {
                    request_id: request_id.clone(),
                },
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // one SSE event per arm, in the protocol's order
    fn handle_frame(
        &mut self,
        frame: &[u8],
        events: &mut Vec<ProviderStreamEvent>,
        _request_id: &RequestId,
    ) -> Result<(), ProviderError> {
        let text =
            std::str::from_utf8(frame).map_err(|_| protocol("provider SSE frame is not UTF-8"))?;
        let mut event_name = String::new();
        let mut data = String::new();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(value) = line.strip_prefix("event:") {
                value.trim().clone_into(&mut event_name);
            }
            if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
        if data.is_empty() {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&data)
            .map_err(|_| protocol("provider SSE data is invalid JSON"))?;
        match event_name.as_str() {
            "message_start" => {
                // The prompt is the uncached input plus what was read from or
                // written to the cache, as prime-agent counts a context's tokens.
                self.prompt_tokens = [
                    "input_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .filter_map(|field| value["message"]["usage"][field].as_u64())
                .sum();
                self.cache_read_tokens = value["message"]["usage"]["cache_read_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
                self.cache_write_tokens = value["message"]["usage"]["cache_creation_input_tokens"]
                    .as_u64()
                    .unwrap_or(0);
            }
            "content_block_start" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let block = &value["content_block"];
                if block["type"] == "tool_use" {
                    let id = block["id"].as_str().unwrap_or("tool-call").to_owned();
                    let name = block["name"].as_str().unwrap_or_default().to_owned();
                    let input = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    let arguments = if input.as_object().is_some_and(serde_json::Map::is_empty) {
                        String::new()
                    } else {
                        input.to_string()
                    };
                    self.tools.insert(index, (id.clone(), name.clone()));
                    events.push(ProviderStreamEvent::tool_delta(id, name, arguments));
                }
            }
            "content_block_delta" => {
                let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
                let delta = &value["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        if let Some(text) = delta["text"].as_str() {
                            events.push(ProviderStreamEvent::text(text));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(text) = delta["thinking"].as_str() {
                            events.push(ProviderStreamEvent::thinking(text));
                        }
                    }
                    "signature_delta" => {
                        if let Some(signature) = delta["signature"].as_str() {
                            events.push(ProviderStreamEvent::ThinkingSignature {
                                signature: signature.to_owned(),
                            });
                        }
                    }
                    "input_json_delta" => {
                        if let Some((id, name)) = self.tools.get(&index)
                            && let Some(arguments) = delta["partial_json"].as_str()
                        {
                            events.push(ProviderStreamEvent::tool_delta(
                                id.clone(),
                                name.clone(),
                                arguments,
                            ));
                        }
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                self.output_tokens = value
                    .pointer("/usage/output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(self.output_tokens);
                if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    events.push(ProviderStreamEvent::completed(reason));
                }
            }
            "message_stop" => self.saw_message_stop = true,
            "error" => return Err(protocol("provider returned an Anthropic stream error")),
            _ => {}
        }
        Ok(())
    }
}

fn protocol(message: &str) -> ProviderError {
    ProviderError::new(ErrorCode::ProviderProtocol, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProviderMessage;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };

    fn fixture_response(response: String) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("adapter connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let mut header_end = None;
            let mut content_length: Option<usize> = None;
            let mut chunked = false;
            loop {
                let count = stream.read(&mut buffer).expect("request bytes");
                assert_ne!(count, 0, "request body completes");
                request.extend_from_slice(&buffer[..count]);
                if header_end.is_none()
                    && let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                {
                    header_end = Some(end + 4);
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse().ok());
                    chunked = headers.lines().any(|line| {
                        line.starts_with("transfer-encoding:") && line.contains("chunked")
                    });
                }
                if let Some(end) = header_end
                    && (content_length.is_some_and(|length| request.len() >= end + length)
                        || (chunked && request.windows(5).any(|window| window == b"0\r\n\r\n")))
                {
                    break;
                }
            }
            write!(stream, "{response}").expect("fixture response");
        });
        (format!("http://{address}/v1/messages"), thread)
    }

    /// A batch of tool results goes back as one user message, and the cache has a
    /// breakpoint on the system prompt, the last tool, the last user message and
    /// the user message before it - where the previous request wrote its entry.
    #[test]
    fn tool_results_share_one_message_and_two_user_breakpoints_are_set() {
        let calls = vec![
            crate::ProviderToolCall::new("call-a", "read_file", r#"{"path":"a.rs"}"#),
            crate::ProviderToolCall::new("call-b", "read_file", r#"{"path":"b.rs"}"#),
        ];
        let request = crate::ProviderRequest::new(
            harness_types::RequestId::generate(),
            "claude-opus-5-5",
            vec![
                ProviderMessage::new(crate::MessageRole::System, "stable system prompt"),
                ProviderMessage::new(crate::MessageRole::User, "read both files"),
                ProviderMessage::assistant_with_calls("", calls),
                ProviderMessage::tool_result("call-a", "fn a() {}"),
                ProviderMessage::tool_result("call-b", "fn b() {}"),
            ],
        );
        let body = AnthropicMessagesAdapter::request_body(&request, None);
        let messages = body["messages"].as_array().expect("messages");
        assert_eq!(messages.len(), 3, "{body:#}");
        let results = messages[2]["content"].as_array().expect("blocks");
        assert_eq!(results.len(), 2, "both results in one message: {body:#}");
        assert!(results.iter().all(|block| block["type"] == "tool_result"));
        assert_eq!(
            results[1]["cache_control"]["type"], "ephemeral",
            "the moving breakpoint: {body:#}"
        );
        assert!(results[0].get("cache_control").is_none(), "{body:#}");
        assert_eq!(
            messages[0]["content"][0]["cache_control"]["type"], "ephemeral",
            "the previous request's breakpoint: {body:#}"
        );
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        let marks = body.to_string().matches("cache_control").count();
        assert!(
            marks <= 4,
            "the API takes at most four breakpoints: {marks}"
        );
    }

    fn adapter(endpoint: String) -> AnthropicMessagesAdapter {
        AnthropicMessagesAdapter::new(
            endpoint,
            Arc::new(super::super::StaticCredentialResolver::new("fixture-token")),
            ModelCapabilities {
                provider_id: "anthropic".to_owned(),
                model: "fixture-model".to_owned(),
                supports_streaming: true,
                supports_tools: true,
                fixture: false,
            },
        )
        .expect("fixture adapter")
    }

    #[tokio::test]
    async fn g03_anthropic_stream_maps_tool_use_to_tool_call_delta() {
        let _fixture_lock = crate::LOOPBACK_FIXTURE_LOCK.lock().await;
        let body = concat!(
            "event: message_start\ndata: {\"message\":{\"usage\":{\"input_tokens\":5}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"read_file\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"a.rs\\\"}\"}}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":2}}\n\n",
            "event: message_stop\ndata: {}\n\n"
        );
        let (endpoint, fixture) = fixture_response(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let result = adapter(endpoint)
            .stream(
                ProviderRequest::new(
                    RequestId::generate(),
                    "fixture-model",
                    vec![ProviderMessage::new(super::super::MessageRole::User, "go")],
                ),
                CancellationToken::new(),
            )
            .await;
        fixture.join().expect("fixture thread");
        let events = result.expect("Anthropic fixture stream");
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, ProviderStreamEvent::Started { .. }))
                .count(),
            1,
            "one Started opens the call; a second would read as a retry: {events:?}"
        );
        assert!(events.iter().any(
            |event| matches!(event, ProviderStreamEvent::TextDelta { text } if text == "hello")
        ));
        assert!(events.iter().any(|event| matches!(event, ProviderStreamEvent::ToolCallDelta { call_id, name, arguments } if call_id == "call-1" && name == "read_file" && arguments.contains("a.rs"))));
    }

    #[tokio::test]
    async fn g03_anthropic_429_http_date_is_bounded() {
        let _fixture_lock = crate::LOOPBACK_FIXTURE_LOCK.lock().await;
        let retry_date = (chrono::Utc::now() + chrono::Duration::seconds(120))
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        let (endpoint, fixture) = fixture_response(format!(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: {retry_date}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        let result = adapter(endpoint)
            .stream(
                ProviderRequest::new(RequestId::generate(), "fixture-model", vec![]),
                CancellationToken::new(),
            )
            .await;
        let error = result.expect_err("429 response");
        // The error keeps the provider's reset (bounded to a week): the runtime
        // caps its own retry sleep at 30 seconds, and a reset beyond the usage
        // wait is what parks the session (prime-agent's quota park).
        let kept = error.retry_after().expect("the HTTP date is read");
        assert!(
            kept > Duration::from_secs(100) && kept <= Duration::from_mins(2),
            "a future HTTP date is kept as the wait until it: {kept:?}"
        );
        fixture.join().expect("fixture thread");
    }

    #[tokio::test]
    async fn g03_anthropic_truncated_stream_is_rejected() {
        let _fixture_lock = crate::LOOPBACK_FIXTURE_LOCK.lock().await;
        let body = "event: message_start\ndata: {\"message\":{}}\n\n";
        let (endpoint, fixture) = fixture_response(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ));
        let result = adapter(endpoint)
            .stream(
                ProviderRequest::new(RequestId::generate(), "fixture-model", vec![]),
                CancellationToken::new(),
            )
            .await;
        fixture.join().expect("fixture thread");
        assert!(
            result
                .expect_err("truncated response must fail")
                .to_string()
                .contains("before message_stop")
        );
    }
}
