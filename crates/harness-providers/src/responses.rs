//! `OpenAI` Responses transport adapter, after prime-agent's `pi-ai`
//! (`providers/openai-responses.ts`, `providers/openai-codex-responses.ts`,
//! `providers/openai-responses-shared.ts`).
//!
//! One adapter serves two endpoints that speak the same wire format:
//!
//! * the public API (`https://api.openai.com/v1/responses`, `OpenCode`'s
//!   `/v1/responses`), authorized with an API key;
//! * `ChatGPT`'s Codex backend (`https://chatgpt.com/backend-api/codex/responses`),
//!   authorized with a sign-in token, which also names the account the token
//!   belongs to and never stores a response (`store: false`).
//!
//! Reasoning comes back as an encrypted item. It is kept, serialized, as the
//! assistant message's reasoning signature and sent back on the next request of
//! the turn, so a reasoning model keeps its chain of thought across tool calls.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use base64::Engine as _;
use futures_util::StreamExt;
use harness_types::ErrorCode;
use reqwest::Client;
use serde_json::{Value, json};

use crate::{
    CancellationToken, CredentialResolver, MessageRole, ModelCapabilities, ModelProvider,
    ProviderError, ProviderFuture, ProviderRequest, ProviderStreamEvent, ThinkingLevel,
};

const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;

/// Where the JWT of a `ChatGPT` sign-in keeps the account id.
const JWT_CLAIM_PATH: &str = "https://api.openai.com/auth";

/// Which Responses endpoint the adapter talks to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponsesFlavor {
    /// The public API or a compatible gateway, with an API key.
    Api,
    /// `ChatGPT`'s Codex backend, with a sign-in token.
    Codex {
        /// Sent as `originator`: which client is calling.
        originator: String,
    },
}

/// What one adapter sends besides the conversation.
#[derive(Clone, Debug, Default)]
pub struct ResponsesOptions {
    /// The session's reasoning effort; `None` or `Off` sends no reasoning field.
    pub reasoning: Option<ThinkingLevel>,
    /// Extra headers, such as `OpenCode`'s session header.
    pub headers: Vec<(String, String)>,
    /// Identifies the conversation, for prompt caching and the Codex session header.
    pub session_id: Option<String>,
}

pub struct OpenAiResponsesAdapter {
    endpoint: String,
    credentials: Arc<dyn CredentialResolver>,
    capabilities: ModelCapabilities,
    client: Client,
    flavor: ResponsesFlavor,
    options: ResponsesOptions,
}

impl OpenAiResponsesAdapter {
    pub fn new(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
        flavor: ResponsesFlavor,
        options: ResponsesOptions,
    ) -> Result<Self, ProviderError> {
        let endpoint = endpoint.into();
        super::validate_endpoint(&endpoint)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(super::DEFAULT_CONNECT_TIMEOUT_SECONDS))
            .timeout(Duration::from_secs(
                super::DEFAULT_REQUEST_TIMEOUT_SECONDS * 3,
            ))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| protocol("provider client is not buildable"))?;
        Ok(Self {
            endpoint,
            credentials,
            capabilities,
            client,
            flavor,
            options,
        })
    }

    fn request_body(&self, request: &ProviderRequest) -> Value {
        let (instructions, input) = input_items(&request.messages);
        let codex = matches!(self.flavor, ResponsesFlavor::Codex { .. });
        let mut body = json!({
            "model": request.model,
            "input": input,
            "stream": true,
            "store": false,
        });
        let instructions = instructions.join("\n\n");
        if !instructions.is_empty() {
            body["instructions"] = json!(instructions);
        } else if codex {
            body["instructions"] = json!("You are a helpful assistant.");
        }
        if let Some(max_tokens) = request.max_output_tokens
            && !codex
        {
            body["max_output_tokens"] = json!(max_tokens);
        }
        let tools = request
            .tool_schemas
            .iter()
            .filter_map(responses_tool)
            .collect::<Vec<_>>();
        if !tools.is_empty() {
            body["tools"] = json!(tools);
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(true);
        }
        if let Some(session) = &self.options.session_id {
            body["prompt_cache_key"] = json!(session);
        }
        if let Some(level) = self
            .options
            .reasoning
            .filter(|level| *level != ThinkingLevel::Off)
        {
            let effort = match level {
                ThinkingLevel::Max => "xhigh",
                other => other.as_str(),
            };
            body["reasoning"] = json!({"effort": effort, "summary": "auto"});
            body["include"] = json!(["reasoning.encrypted_content"]);
        }
        if codex {
            body["text"] = json!({"verbosity": "low"});
        }
        body
    }

    fn headers(&self, token: &str) -> Result<Vec<(String, String)>, ProviderError> {
        let mut headers = self.options.headers.clone();
        headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
        headers.push(("accept".to_owned(), "text/event-stream".to_owned()));
        if let ResponsesFlavor::Codex { originator } = &self.flavor {
            let account = account_id(token).ok_or_else(|| {
                ProviderError::new(
                    ErrorCode::SecretNotGranted,
                    "the ChatGPT sign-in token names no account; log in again with /login",
                )
            })?;
            headers.push(("chatgpt-account-id".to_owned(), account));
            headers.push(("originator".to_owned(), originator.clone()));
            headers.push((
                "OpenAI-Beta".to_owned(),
                "responses=experimental".to_owned(),
            ));
            if let Some(session) = &self.options.session_id {
                headers.push(("session_id".to_owned(), session.clone()));
            }
        }
        Ok(headers)
    }
}

/// The system prompt, and the conversation as Responses input items.
fn input_items(messages: &[crate::ProviderMessage]) -> (Vec<String>, Vec<Value>) {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in messages {
        match message.role {
            MessageRole::System => instructions.push(message.content.clone()),
            MessageRole::User => {
                let mut content = vec![json!({"type": "input_text", "text": message.content})];
                for image in &message.attachments {
                    content.push(json!({"type": "input_image", "image_url": image.url()}));
                }
                input.push(json!({"role": "user", "content": content}));
            }
            MessageRole::Assistant => {
                if let Some(item) = message
                    .reasoning
                    .as_ref()
                    .and_then(|reasoning| reasoning.signature.as_deref())
                    .and_then(|signature| serde_json::from_str::<Value>(signature).ok())
                    .filter(|item| item["type"] == "reasoning")
                {
                    input.push(item);
                }
                if !message.content.is_empty() {
                    input.push(json!({
                            "type": "message",
                            "role": "assistant",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": message.content, "annotations": []}],
                        }));
                }
                for call in &message.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.call_id,
                        "name": call.name,
                        "arguments": call.arguments,
                    }));
                }
            }
            MessageRole::Tool => {
                let call_id = message.tool_call_id.as_deref().unwrap_or("missing-call-id");
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": message.content,
                }));
                // An image a tool returned goes in as the user turn that follows.
                if !message.attachments.is_empty() {
                    let mut content = vec![
                        json!({"type": "input_text", "text": "Attached image(s) from tool result:"}),
                    ];
                    for image in &message.attachments {
                        content.push(json!({"type": "input_image", "image_url": image.url()}));
                    }
                    input.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
    (instructions, input)
}

/// The account id a `ChatGPT` sign-in token carries in its JWT claims.
#[must_use]
pub fn account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    claims[JWT_CLAIM_PATH]["chatgpt_account_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// A Chat Completions tool schema in the Responses shape.
fn responses_tool(schema: &Value) -> Option<Value> {
    let function = schema.get("function").unwrap_or(schema);
    let name = function.get("name")?.as_str()?;
    let mut tool = json!({
        "type": "function",
        "name": name,
        "parameters": function.get("parameters").cloned().unwrap_or_else(|| json!({"type": "object"})),
        "strict": Value::Null,
    });
    if let Some(description) = function.get("description") {
        tool["description"] = description.clone();
    }
    Some(tool)
}

impl ModelProvider for OpenAiResponsesAdapter {
    fn capabilities(&self) -> ModelCapabilities {
        self.capabilities.clone()
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let endpoint = self.endpoint.clone();
        let client = self.client.clone();
        let credentials = Arc::clone(&self.credentials);
        let body = self.request_body(&request);
        let token = credentials.resolve();
        let headers = token.and_then(|token| self.headers(&token));
        Box::pin(async move {
            let headers = headers?;
            let mut post = client.post(endpoint).json(&body);
            for (name, value) in headers {
                post = post.header(name, value);
            }
            let response = tokio::select! {
                result = post.send() => result.map_err(|error| {
                    let error = error.without_url();
                    ProviderError::new(
                        if error.is_timeout() { ErrorCode::ProcessTimedOut } else { ErrorCode::ServiceUnavailable },
                        format!("provider request failed: {error}"),
                    )
                })?,
                () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider request canceled")),
            };
            if !response.status().is_success() {
                return Err(super::http_response_error(response).await);
            }
            let mut stream = response.bytes_stream();
            let mut decoder = ResponsesSseDecoder::default();
            let mut events = vec![ProviderStreamEvent::Started {
                request_id: request.request_id.clone(),
            }];
            loop {
                let item = tokio::select! {
                    item = stream.next() => item,
                    () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider stream canceled")),
                };
                let Some(item) = item else {
                    break;
                };
                let chunk = item.map_err(|error| {
                    protocol(&format!("provider stream failed: {}", error.without_url()))
                })?;
                decoder.feed(&chunk, &mut events)?;
            }
            decoder.finish(&mut events)?;
            Ok(events)
        })
    }
}

#[derive(Default)]
struct ResponsesSseDecoder {
    buffer: Vec<u8>,
    total_bytes: usize,
    finished: bool,
    /// Output item id -> (call id, tool name), for argument deltas.
    calls: BTreeMap<String, (String, String)>,
    saw_call: bool,
}

impl ResponsesSseDecoder {
    fn feed(
        &mut self,
        bytes: &[u8],
        events: &mut Vec<ProviderStreamEvent>,
    ) -> Result<(), ProviderError> {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        if self.total_bytes > MAX_STREAM_BYTES {
            return Err(protocol("provider stream exceeds the size limit"));
        }
        self.buffer.extend_from_slice(bytes);
        loop {
            let end = self
                .buffer
                .windows(2)
                .position(|pair| pair == b"\n\n")
                .map(|end| (end, 2))
                .or_else(|| {
                    self.buffer
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .map(|end| (end, 4))
                });
            let Some((end, separator)) = end else {
                break;
            };
            let frame = self.buffer.drain(..end + separator).collect::<Vec<_>>();
            self.frame(&frame, events)?;
        }
        Ok(())
    }

    fn finish(&mut self, events: &mut Vec<ProviderStreamEvent>) -> Result<(), ProviderError> {
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let rest = std::mem::take(&mut self.buffer);
            self.frame(&rest, events)?;
        }
        if !self.finished {
            return Err(protocol("provider stream ended before response.completed"));
        }
        Ok(())
    }

    fn frame(
        &mut self,
        frame: &[u8],
        events: &mut Vec<ProviderStreamEvent>,
    ) -> Result<(), ProviderError> {
        let text =
            std::str::from_utf8(frame).map_err(|_| protocol("provider SSE frame is not UTF-8"))?;
        let mut data = String::new();
        for line in text.lines() {
            if let Some(value) = line.trim_end_matches('\r').strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.trim_start());
            }
        }
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let value: Value = serde_json::from_str(&data)
            .map_err(|_| protocol("provider SSE data is invalid JSON"))?;
        match value["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    events.push(ProviderStreamEvent::text(delta));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    events.push(ProviderStreamEvent::thinking(delta));
                }
            }
            "response.reasoning_summary_part.done" => {
                events.push(ProviderStreamEvent::thinking("\n\n"));
            }
            "response.output_item.added" => {
                let item = &value["item"];
                if item["type"] == "function_call" {
                    let item_id = item["id"].as_str().unwrap_or_default().to_owned();
                    let call_id = item["call_id"].as_str().unwrap_or(&item_id).to_owned();
                    let name = item["name"].as_str().unwrap_or_default().to_owned();
                    let arguments = item["arguments"].as_str().unwrap_or_default().to_owned();
                    self.calls.insert(item_id, (call_id.clone(), name.clone()));
                    self.saw_call = true;
                    events.push(ProviderStreamEvent::tool_delta(call_id, name, arguments));
                }
            }
            "response.function_call_arguments.delta" => {
                let item_id = value["item_id"].as_str().unwrap_or_default();
                if let Some((call_id, name)) = self.calls.get(item_id)
                    && let Some(delta) = value["delta"].as_str()
                {
                    events.push(ProviderStreamEvent::tool_delta(
                        call_id.clone(),
                        name.clone(),
                        delta,
                    ));
                }
            }
            "response.output_item.done" => {
                let item = &value["item"];
                if item["type"] == "reasoning" {
                    // Kept whole, so the next request of the turn can send it back.
                    let kept = json!({
                        "type": "reasoning",
                        "id": item["id"],
                        "summary": item.get("summary").cloned().unwrap_or_else(|| json!([])),
                        "encrypted_content": item["encrypted_content"],
                    });
                    events.push(ProviderStreamEvent::ThinkingSignature {
                        signature: kept.to_string(),
                    });
                }
            }
            "response.completed" | "response.done" | "response.incomplete" => {
                let response = &value["response"];
                let usage = &response["usage"];
                let input = usage["input_tokens"].as_u64().unwrap_or(0);
                let output = usage["output_tokens"].as_u64().unwrap_or(0);
                let total = usage["total_tokens"].as_u64().unwrap_or(input + output);
                events.push(ProviderStreamEvent::usage(input, output, total));
                let reason = if value["type"] == "response.incomplete" {
                    "length"
                } else if self.saw_call {
                    "tool_calls"
                } else {
                    "stop"
                };
                events.push(ProviderStreamEvent::completed(reason));
                self.finished = true;
            }
            "response.failed" | "error" => {
                let message = value
                    .pointer("/response/error/message")
                    .or_else(|| value.pointer("/error/message"))
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("the provider reported a failure");
                return Err(ProviderError::new(
                    ErrorCode::ServiceUnavailable,
                    format!("provider error: {message}"),
                ));
            }
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
    use crate::{ProviderMessage, ProviderToolCall, Reasoning, StaticCredentialResolver};
    use harness_types::RequestId;

    fn adapter(flavor: ResponsesFlavor) -> OpenAiResponsesAdapter {
        OpenAiResponsesAdapter::new(
            "https://api.openai.com/v1/responses",
            Arc::new(StaticCredentialResolver::new("sk-test")),
            ModelCapabilities {
                provider_id: "openai".to_owned(),
                model: "gpt-5".to_owned(),
                supports_streaming: true,
                supports_tools: true,
                fixture: false,
            },
            flavor,
            ResponsesOptions {
                reasoning: Some(ThinkingLevel::High),
                headers: Vec::new(),
                session_id: Some("session-1".to_owned()),
            },
        )
        .expect("adapter")
    }

    #[test]
    fn a_turn_is_sent_as_responses_input_items() {
        let reasoning_item =
            json!({"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "enc"});
        let request = ProviderRequest::new(
            RequestId::generate(),
            "gpt-5",
            vec![
                ProviderMessage::new(MessageRole::System, "be brief"),
                ProviderMessage::new(MessageRole::User, "list files"),
                ProviderMessage::assistant_with_calls(
                    "",
                    vec![ProviderToolCall::new("call_1", "list_files", "{\"path\":\".\"}")],
                )
                .with_reasoning(Some(Reasoning {
                    text: "thinking".to_owned(),
                    signature: Some(reasoning_item.to_string()),
                })),
                ProviderMessage::tool_result("call_1", "a.rs"),
            ],
        )
        .with_tool_schemas(vec![json!({
            "type": "function",
            "function": {"name": "list_files", "description": "list", "parameters": {"type": "object"}}
        })]);
        let body = adapter(ResponsesFlavor::Api).request_body(&request);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["reasoning"]["effort"], "high");
        let input = body["input"].as_array().expect("input");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["name"], "list_files");
    }

    #[test]
    fn a_stream_becomes_text_calls_reasoning_and_a_reason() {
        let stream = [
            json!({"type": "response.output_item.added", "item": {"type": "reasoning", "id": "rs_1"}}),
            json!({"type": "response.reasoning_summary_text.delta", "delta": "plan"}),
            json!({"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_1", "encrypted_content": "enc"}}),
            json!({"type": "response.output_text.delta", "delta": "hi"}),
            json!({"type": "response.output_item.added", "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "read_file", "arguments": ""}}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"path\":"}),
            json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "\"a\"}"}),
            json!({"type": "response.completed", "response": {"usage": {"input_tokens": 3, "output_tokens": 4, "total_tokens": 7}}}),
        ];
        let mut bytes = Vec::new();
        for event in stream {
            bytes.extend_from_slice(format!("event: x\ndata: {event}\n\n").as_bytes());
        }
        let mut decoder = ResponsesSseDecoder::default();
        let mut events = Vec::new();
        // Split mid-frame to prove frames are reassembled.
        let (first, second) = bytes.split_at(bytes.len() / 2);
        decoder.feed(first, &mut events).expect("first half");
        decoder.feed(second, &mut events).expect("second half");
        decoder.finish(&mut events).expect("complete");
        let response = crate::assemble_stream(&events).expect("assembled");
        assert_eq!(response.text, "hi");
        assert_eq!(response.reasoning, "plan");
        assert!(response.reasoning_signature.expect("kept").contains("enc"));
        assert_eq!(response.tool_calls[0].call_id, "call_1");
        assert_eq!(response.tool_calls[0].arguments, "{\"path\":\"a\"}");
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn a_stream_cut_before_completion_is_refused() {
        let mut decoder = ResponsesSseDecoder::default();
        let mut events = Vec::new();
        decoder
            .feed(
                b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n",
                &mut events,
            )
            .expect("frame");
        assert!(decoder.finish(&mut events).is_err());
    }

    #[test]
    fn a_codex_token_names_its_account() {
        let claims = json!({JWT_CLAIM_PATH: {"chatgpt_account_id": "acct_1"}});
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        let token = format!("header.{payload}.signature");
        assert_eq!(account_id(&token).as_deref(), Some("acct_1"));
        let headers = adapter(ResponsesFlavor::Codex {
            originator: "ha".to_owned(),
        })
        .headers(&token)
        .expect("headers");
        assert!(headers.contains(&("chatgpt-account-id".to_owned(), "acct_1".to_owned())));
        assert!(headers.contains(&("session_id".to_owned(), "session-1".to_owned())));
        assert!(account_id("not-a-jwt").is_none());
    }
}
