#![forbid(unsafe_code)]

//! Provider boundary for the P2 runtime.
//! Providers normalize transport only; execution and durable state remain in
//! the runtime layer.

use std::{
    collections::{BTreeMap, BTreeSet},
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

#[cfg(test)]
pub(crate) static LOOPBACK_FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub mod anthropic;
pub mod responses;
mod streaming;
pub mod thinking;
pub use responses::{OpenAiResponsesAdapter, ResponsesFlavor, ResponsesOptions};
pub use streaming::{ProviderEventStream, collect_events};
pub use thinking::{Thinking, ThinkingFormat, ThinkingLevel};

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

/// What a host knows about one provider parameter.
///
/// `Unknown` is not `Unsupported`: it means no claim was made, so the parameter
/// may be sent — but it is never evidence of compatibility (ADR-N04).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityClaim {
    Supported,
    Unsupported,
    Unknown,
}

impl CapabilityClaim {
    #[must_use]
    pub const fn from_flag(supported: bool) -> Self {
        if supported {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }

    #[must_use]
    pub const fn is_unsupported(self) -> bool {
        matches!(self, Self::Unsupported)
    }
}

/// The capability claims one provider/model pair makes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityMatrix {
    pub provider_id: String,
    pub model: String,
    pub streaming: CapabilityClaim,
    pub tools: CapabilityClaim,
    pub images: CapabilityClaim,
}

impl CapabilityMatrix {
    /// Claims derived from the boolean capability record the runtime already has:
    /// a `false` flag is an explicit "unsupported", never an "unknown".
    #[must_use]
    pub fn from_capabilities(capabilities: &ModelCapabilities) -> Self {
        Self {
            provider_id: capabilities.provider_id.clone(),
            model: capabilities.model.clone(),
            streaming: CapabilityClaim::from_flag(capabilities.supports_streaming),
            tools: CapabilityClaim::from_flag(capabilities.supports_tools),
            images: CapabilityClaim::Unknown,
        }
    }

    /// The `DeepSeek` claims measured against the official API docs at M2 time:
    /// `deepseek-flash` and `deepseek-v4-pro` speak the `OpenAI` chat format with
    /// streaming and tool calls; thinking mode defaults on, which this app turns
    /// off explicitly. Images are only claimed for the model that documents them,
    /// and `Unknown` is used where this host has no claim.
    #[must_use]
    pub fn deepseek_documented(model: impl Into<String>) -> Self {
        let model = model.into();
        let vision = model.contains("flash") || model.starts_with("deepseek-chat");
        Self {
            provider_id: "deepseek".to_owned(),
            model,
            streaming: CapabilityClaim::Supported,
            tools: CapabilityClaim::Supported,
            images: if vision {
                CapabilityClaim::Supported
            } else {
                CapabilityClaim::Unknown
            },
        }
    }

    /// Refuse a request that needs a parameter the provider explicitly lacks.
    ///
    /// `Unknown` passes: the request may still succeed, and the caller must not
    /// read the pass as proof of support.
    pub fn validate(&self, request: &ProviderRequest) -> Result<(), ProviderError> {
        if !request.tool_schemas.is_empty() && self.tools.is_unsupported() {
            return Err(ProviderError::new(
                ErrorCode::IncompatibleService,
                format!(
                    "provider {} model {} does not support tool calls",
                    self.provider_id, self.model
                ),
            ));
        }
        if self.images.is_unsupported()
            && request
                .messages
                .iter()
                .any(|message| !message.attachments.is_empty())
        {
            return Err(ProviderError::new(
                ErrorCode::IncompatibleService,
                format!(
                    "provider {} model {} does not accept images",
                    self.provider_id, self.model
                ),
            ));
        }
        Ok(())
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
    /// Images the model is shown together with this message.
    ///
    /// Only a `user` message may carry them: the API accepts image blocks on that role
    /// alone and refuses any other role. `to_wire` therefore keeps the text shape for
    /// every other role rather than sending a request that cannot succeed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ImageAttachment>,
    /// The calls an assistant message asked for (canonical model only).
    ///
    /// This is what the transcript validator correlates tool results against. The
    /// Chat Completions encoding deliberately does not carry it mid-conversation
    /// (ADR-N04), so the field is canonical, not wire.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ProviderToolCall>,
    /// The call a tool result answers (canonical model only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// The private reasoning an assistant message came with, kept only for the
    /// turn so a thinking model gets it back (`DeepSeek`'s `reasoning_content`,
    /// Anthropic's signed `thinking` block). Never serialized: reasoning is not
    /// durable.
    #[serde(skip)]
    pub reasoning: Option<Reasoning>,
}

/// One assistant message's private reasoning.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Reasoning {
    pub text: String,
    /// Anthropic's signature over the thinking block, when the provider sent one.
    pub signature: Option<String>,
}

/// One call an assistant message asked for, as the canonical model keeps it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

impl ProviderToolCall {
    #[must_use]
    pub fn new(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            call_id: call_id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }
}

/// Where the bytes of an attached image come from.
///
/// The API accepts both shapes in the same block position: an inline `data:` URL, and a
/// public `http(s)` link the provider downloads itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageSource {
    /// The bytes travel inside the request, base64 of the file without a `data:` prefix.
    Inline {
        /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
        media_type: String,
        data_base64: String,
    },
    /// A link the provider fetches, for a picture this app never holds the bytes of.
    Remote {
        /// The `http(s)` URL, at most [`MAX_REMOTE_URL_CHARS`] long.
        url: String,
    },
}

/// Longest `http(s)` link the API accepts for a remote image.
pub const MAX_REMOTE_URL_CHARS: usize = 8192;

impl ImageSource {
    /// A public link, when the API could fetch it at all: `http(s)`, and short enough.
    ///
    /// `None` is a refusal the caller turns into a reason — a link that is not fetched by
    /// anyone must not look like an image that was attached.
    #[must_use]
    pub fn remote(url: impl Into<String>) -> Option<Self> {
        let url = url.into();
        let scheme = url.starts_with("http://") || url.starts_with("https://");
        (scheme && url.chars().count() <= MAX_REMOTE_URL_CHARS).then_some(Self::Remote { url })
    }

    /// The `image_url.url` value: a data URL, or the link the provider downloads.
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            Self::Inline {
                media_type,
                data_base64,
            } => format!("data:{media_type};base64,{data_base64}"),
            Self::Remote { url } => url.clone(),
        }
    }
}

/// One image carried by a user message.
///
/// The API accepts PNG, JPEG, GIF and WebP, and detects the format from the bytes
/// rather than from a file name or a declared type — so an inline attachment carries the
/// media type the bytes really are, and the wire URL is built in exactly one place.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageAttachment {
    /// Inline bytes, or a link the provider fetches.
    pub source: ImageSource,
    /// One short line naming the image for the transcript and the packet marker.
    pub label: String,
}

impl ImageAttachment {
    /// An image whose bytes this app read and encoded.
    #[must_use]
    pub fn inline(
        media_type: impl Into<String>,
        data_base64: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            source: ImageSource::Inline {
                media_type: media_type.into(),
                data_base64: data_base64.into(),
            },
            label: label.into(),
        }
    }

    /// An image the provider downloads from a public link.
    #[must_use]
    pub fn remote(url: impl Into<String>, label: impl Into<String>) -> Option<Self> {
        Some(Self {
            source: ImageSource::remote(url)?,
            label: label.into(),
        })
    }

    /// The URL this attachment puts in the request.
    #[must_use]
    pub fn url(&self) -> String {
        self.source.url()
    }
}

impl ProviderMessage {
    #[must_use]
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// A user message that shows the model one or more images.
    #[must_use]
    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            attachments: images,
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// An assistant message that asked for the given calls.
    #[must_use]
    pub fn assistant_with_calls(
        content: impl Into<String>,
        tool_calls: Vec<ProviderToolCall>,
    ) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls,
            tool_call_id: None,
            reasoning: None,
        }
    }

    /// A tool result answering one call.
    #[must_use]
    pub fn tool_result(call_id: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Tool,
            content: payload.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
            reasoning: None,
        }
    }

    /// Show the model these images with this message. Only a user message or a tool
    /// result (sent as one) carries them.
    #[must_use]
    pub fn with_attachments(mut self, images: Vec<ImageAttachment>) -> Self {
        self.attachments = images;
        self
    }

    /// Attach the reasoning this assistant message came with.
    #[must_use]
    pub fn with_reasoning(mut self, reasoning: Option<Reasoning>) -> Self {
        self.reasoning = reasoning
            .filter(|reasoning| !reasoning.text.is_empty() || reasoning.signature.is_some());
        self
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
    /// said. A message with images uses the block form of `content` instead of a
    /// string, because that is the only shape the API reads images from.
    #[must_use]
    pub fn to_wire(&self) -> Value {
        let text = match self.role {
            MessageRole::Tool => format!("[tool result]\n{}", self.content),
            MessageRole::System | MessageRole::User | MessageRole::Assistant => {
                self.content.clone()
            }
        };
        // A tool result is sent as a user message, so the images a tool returned (the
        // `attach_image` skill's) ride with it the same way.
        if !self.attachments.is_empty()
            && matches!(self.role, MessageRole::User | MessageRole::Tool)
        {
            let mut blocks = vec![json!({ "type": "text", "text": text })];
            for image in &self.attachments {
                blocks.push(json!({
                    "type": "image_url",
                    // `auto` lets the provider pick the detail level; its own default is
                    // the same, and pinning `high` would spend tokens on every small icon.
                    "image_url": { "url": image.url(), "detail": "auto" },
                }));
            }
            return json!({ "role": "user", "content": blocks });
        }
        match self.role {
            MessageRole::Tool | MessageRole::User => json!({ "role": "user", "content": text }),
            MessageRole::System => json!({ "role": "system", "content": text }),
            MessageRole::Assistant => json!({ "role": "assistant", "content": text }),
        }
    }
}

/// The `messages` array as the API accepts it.
#[must_use]
pub fn wire_messages(messages: &[ProviderMessage]) -> Vec<Value> {
    messages.iter().map(ProviderMessage::to_wire).collect()
}

/// The `messages` array for a thinking endpoint that wants every assistant message's
/// `reasoning_content` back - an empty one when none was kept, as `pi-ai` sends for
/// `DeepSeek` (`requiresReasoningContentOnAssistantMessages`).
#[must_use]
pub fn wire_messages_with_reasoning(messages: &[ProviderMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut wire = message.to_wire();
            if message.role == MessageRole::Assistant {
                wire["reasoning_content"] = json!(
                    message
                        .reasoning
                        .as_ref()
                        .map_or("", |reasoning| reasoning.text.as_str())
                );
            }
            wire
        })
        .collect()
}

/// Validate the paired transcript before anything is dispatched.
///
/// The canonical model carries call identity, so the host can refuse a
/// transcript whose tool results cannot be correlated instead of sending it and
/// guessing later:
///
/// - a tool result must name a call an assistant message announced **earlier**;
/// - a call id may appear once as a call and be answered once;
/// - a tool result may not precede its call.
///
/// The check is about the visible transcript, not about provider behaviour: an
/// invalid transcript is a host bug and is refused before the network call.
pub fn validate_transcript(messages: &[ProviderMessage]) -> Result<(), ProviderError> {
    let mut announced: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    let mut answered: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for (index, message) in messages.iter().enumerate() {
        for call in &message.tool_calls {
            if call.call_id.trim().is_empty() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    format!("assistant message {index} has a call without an id"),
                ));
            }
            if !announced.insert(call.call_id.as_str()) {
                return Err(ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    format!(
                        "call id {} is announced twice in the transcript",
                        call.call_id
                    ),
                ));
            }
        }
        let Some(call_id) = message.tool_call_id.as_deref() else {
            continue;
        };
        if !announced.contains(call_id) {
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                format!("tool result references call {call_id} before it was announced"),
            ));
        }
        if !answered.insert(call_id) {
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                format!("call {call_id} is answered twice"),
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProviderRequest {
    pub request_id: RequestId,
    pub model: String,
    pub messages: Vec<ProviderMessage>,
    /// Optional per-request output cap, used for bounded nested sampling calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
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
            max_output_tokens: None,
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

    #[must_use]
    pub fn with_max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
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
    /// Private reasoning delta. It is display-only and must not be appended to a
    /// plain transcript or durable conversation text.
    ThinkingDelta {
        text: String,
    },
    /// Anthropic's signature over the thinking block, sent back with it.
    ThinkingSignature {
        signature: String,
    },
    ToolCallDelta {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// Token accounting the provider reported for this call.
    ///
    /// A usage-only frame carries no choice, so it is its own event instead of
    /// being dropped or mistaken for the terminal one.
    Usage {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
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
    pub fn thinking(text: impl Into<String>) -> Self {
        Self::ThinkingDelta { text: text.into() }
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
    pub const fn usage(prompt_tokens: u64, completion_tokens: u64, total_tokens: u64) -> Self {
        Self::Usage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
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
    /// Private reasoning of this response; never serialized, so it stays out of the
    /// response hash and every durable record.
    #[serde(skip)]
    pub reasoning: String,
    #[serde(skip)]
    pub reasoning_signature: Option<String>,
    pub finish_reason: Option<String>,
    pub incomplete_tool_calls: bool,
    pub tool_calls: Vec<NormalizedToolCall>,
}

impl ProviderResponse {
    /// Whether this response may be turned into tool execution.
    ///
    /// A stream that never reached a terminal marker, or that left a call with
    /// unparseable arguments, is reported to the caller but never dispatched: a
    /// truncated stream is not a completed answer (A06).
    #[must_use]
    pub fn is_dispatchable(&self) -> bool {
        self.finish_reason.is_some() && !self.incomplete_tool_calls
    }
}

pub fn assemble_stream(events: &[ProviderStreamEvent]) -> Result<ProviderResponse, ProviderError> {
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut reasoning_signature: Option<String> = None;
    let mut finish_reason = None;
    let mut calls: BTreeMap<String, NormalizedToolCall> = BTreeMap::new();
    for event in events {
        match event {
            ProviderStreamEvent::TextDelta { text: delta } => text.push_str(delta),
            // Token accounting is durable in the event log; it does not change the
            // assembled answer, and `Started` was never part of one.
            ProviderStreamEvent::ThinkingDelta { text: delta } => reasoning.push_str(delta),
            ProviderStreamEvent::ThinkingSignature { signature } => {
                reasoning_signature
                    .get_or_insert_with(String::new)
                    .push_str(signature);
            }
            ProviderStreamEvent::Usage { .. } | ProviderStreamEvent::Started { .. } => {}
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
        reasoning,
        reasoning_signature,
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
    retry_after: Option<Duration>,
}

impl ProviderError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after: None,
        }
    }
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Option<Duration>) -> Self {
        self.retry_after = retry_after;
        self
    }
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
    /// The wait the provider asked for, when it named one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
    /// Whether repeating the call may succeed. The class comes from the stable
    /// code, never from the message.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self.code.retry_class(),
            harness_types::RetryClass::Transient | harness_types::RetryClass::Bounded
        )
    }
}

/// Map one HTTP status from the provider onto the typed error taxonomy.
///
/// The codes come from the provider's own documented table: 400 and 422 are
/// request defects, 401 is authority, 402 is balance, 429/500/503 are transient
/// and worth a bounded retry. A `Retry-After` header is carried on the error so
/// the retry owner can honour it instead of guessing.
#[must_use]
pub fn http_status_error(status: u16, retry_after: Option<Duration>) -> ProviderError {
    let code = match status {
        400 | 422 => ErrorCode::InvalidPayload,
        401 => ErrorCode::MissingAuthority,
        402 => ErrorCode::BudgetExhausted,
        429 | 500 | 502 | 503 | 504 => ErrorCode::ServiceUnavailable,
        _ => ErrorCode::ProviderProtocol,
    };
    ProviderError::new(code, format!("provider returned HTTP {status}"))
        .with_retry_after(retry_after)
}

/// A refused request, with the provider's own reason when its body gives one:
/// "HTTP 400" alone leaves the user no way to tell a bad key from a bad model.
pub(crate) async fn http_response_error(response: reqwest::Response) -> ProviderError {
    let status = response.status().as_u16();
    let retry = retry_after_seconds(response.headers());
    let error = http_status_error(status, retry);
    let detail = response.text().await.ok().and_then(|body| {
        // A JSON error names its message; anything else is shown as sent.
        error_message(&body).or_else(|| {
            let text = body.split_whitespace().collect::<Vec<_>>().join(" ");
            (!text.is_empty()).then(|| text.chars().take(300).collect())
        })
    });
    match detail {
        Some(detail) => ProviderError::new(error.code(), format!("{}: {detail}", error.message))
            .with_retry_after(error.retry_after()),
        None => error,
    }
}

/// The message of a JSON error body, cut to one line.
pub(crate) fn error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let message = value
        .pointer("/error/message")
        .or_else(|| value.pointer("/error"))
        .or_else(|| value.pointer("/detail"))
        .or_else(|| value.get("message"))?;
    let text = message
        .as_str()
        .map_or_else(|| message.to_string(), str::to_owned);
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| text.chars().take(300).collect())
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

/// Default seconds allowed to establish one provider connection.
pub const DEFAULT_CONNECT_TIMEOUT_SECONDS: u64 = 10;
/// Default seconds allowed for one provider call end to end.
pub const DEFAULT_REQUEST_TIMEOUT_SECONDS: u64 = 120;

pub struct OpenAiChatAdapter {
    endpoint: String,
    credentials: Arc<dyn CredentialResolver>,
    capabilities: ModelCapabilities,
    thinking: Option<Thinking>,
    headers: Vec<(String, String)>,
    client: Client,
}

/// Optional provider extension sent alongside the otherwise generic `OpenAI` Chat
/// request. Provider-specific defaults live in config presets; the adapter only
/// serializes the resolved value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpenAiChatOptions {
    /// The session's thinking level and how this endpoint takes it.
    pub thinking: Option<Thinking>,
    /// Extra headers, such as `OpenCode`'s session header.
    pub headers: Vec<(String, String)>,
}

impl OpenAiChatAdapter {
    pub fn new(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
    ) -> Result<Self, ProviderError> {
        Self::with_options(
            endpoint,
            credentials,
            capabilities,
            OpenAiChatOptions::default(),
        )
    }

    pub fn with_options(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
        options: OpenAiChatOptions,
    ) -> Result<Self, ProviderError> {
        Self::with_options_and_timeouts(
            endpoint,
            credentials,
            capabilities,
            options,
            Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECONDS),
            Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECONDS),
        )
    }

    /// Build the adapter with explicit transport timeouts.
    ///
    /// A hung socket must fail as a typed timeout instead of parking a turn
    /// forever; tests use short values instead of sleeping.
    pub fn with_timeouts(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        Self::with_options_and_timeouts(
            endpoint,
            credentials,
            capabilities,
            OpenAiChatOptions::default(),
            connect_timeout,
            request_timeout,
        )
    }

    /// Build the adapter with explicit provider options and transport timeouts.
    pub fn with_options_and_timeouts(
        endpoint: impl Into<String>,
        credentials: Arc<dyn CredentialResolver>,
        capabilities: ModelCapabilities,
        options: OpenAiChatOptions,
        connect_timeout: Duration,
        request_timeout: Duration,
    ) -> Result<Self, ProviderError> {
        let endpoint = chat_completions_endpoint(&endpoint.into());
        if endpoint.trim().is_empty() {
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                "provider endpoint is empty",
            ));
        }
        validate_endpoint(&endpoint)?;
        let client = Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            // No redirects: a 307/308 would re-send the full body — conversation,
            // inline images and tool schemas — to whatever host the response
            // names, and the bearer token would follow a same-origin hop.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    format!("provider client is not buildable: {error}"),
                )
            })?;
        Ok(Self {
            endpoint,
            credentials,
            capabilities,
            thinking: options.thinking,
            headers: options.headers,
            client,
        })
    }
}

/// The Chat Completions body before transport options: messages - with their
/// reasoning when the endpoint wants it back - and the thinking level.
pub(crate) fn chat_body(
    request: &ProviderRequest,
    thinking: Option<&Thinking>,
    provider_id: &str,
) -> Value {
    let messages = if thinking.is_some_and(Thinking::replays_reasoning_content) {
        wire_messages_with_reasoning(&request.messages)
    } else {
        wire_messages(&request.messages)
    };
    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });
    // Sent only when set, as prime-agent does: some gateways refuse a null.
    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }
    if let Some(thinking) = thinking {
        thinking.apply_chat(&mut body, provider_id, &request.model);
    }
    body
}

/// The wait a `Retry-After` header asks for, in seconds; other forms are ignored.
#[must_use]
pub fn retry_after_seconds(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    retry_after_bounded(headers, Duration::from_secs(30))
}

/// Parse delta-seconds or an RFC 7231 HTTP-date and bound the result.
#[must_use]
pub fn retry_after_bounded(
    headers: &reqwest::header::HeaderMap,
    cap: Duration,
) -> Option<Duration> {
    let value = headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim();
    let wait = if let Ok(seconds) = value.parse::<u64>() {
        Duration::from_secs(seconds)
    } else {
        let target = chrono::DateTime::parse_from_rfc2822(value)
            .map(|date| date.with_timezone(&chrono::Utc))
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S GMT")
                    .map(|date| date.and_utc())
            })
            .ok()?;
        let now = chrono::Utc::now();
        target
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO)
    };
    Some(wait.min(cap))
}

/// Refuse an endpoint that would send the API key in cleartext to a remote host.
///
/// `https` is accepted anywhere; plain `http` is accepted only for loopback
/// hosts, which is what local fixtures and loopback proxies use. Any other
/// scheme is refused rather than handed to the HTTP client.
fn validate_endpoint(endpoint: &str) -> Result<(), ProviderError> {
    let url = reqwest::Url::parse(endpoint).map_err(|_| {
        ProviderError::new(
            ErrorCode::ProviderProtocol,
            "provider endpoint is not a valid URL",
        )
    })?;
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        "http" => Err(ProviderError::new(
            ErrorCode::ProviderProtocol,
            "provider endpoint must use https; cleartext http is allowed only for loopback",
        )),
        _ => Err(ProviderError::new(
            ErrorCode::ProviderProtocol,
            "provider endpoint scheme is not supported",
        )),
    }
}

/// Whether the endpoint names the local machine.
fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => address.is_loopback(),
        Ok(std::net::IpAddr::V6(address)) => address.is_loopback(),
        Err(_) => false,
    }
}

impl ModelProvider for OpenAiChatAdapter {
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
        let thinking = self.thinking;
        let provider_id = self.capabilities.provider_id.clone();
        let headers = self.headers.clone();
        Box::pin(async move {
            let token = credentials.resolve()?;
            let mut body = chat_body(&request, thinking.as_ref(), &provider_id);
            if let Some(max_tokens) = request.max_output_tokens {
                body["max_tokens"] = json!(max_tokens);
            }
            if !request.tool_schemas.is_empty()
                && let Some(object) = body.as_object_mut()
            {
                object.insert("tools".to_owned(), Value::Array(request.tool_schemas));
            }
            let response = tokio::select! {
                result = headers
                    .iter()
                    .fold(client.post(endpoint), |post, (name, value)| post.header(name, value))
                    .bearer_auth(token)
                    .json(&body)
                    .send() => result.map_err(|error| {
                    // `without_url` keeps a query-string token or userinfo out of
                    // the message the runtime persists and renders.
                    let error = error.without_url();
                    if error.is_timeout() {
                        ProviderError::new(ErrorCode::ProcessTimedOut, format!("provider request timed out: {error}"))
                    } else {
                        ProviderError::new(ErrorCode::ServiceUnavailable, format!("provider request failed: {error}"))
                    }
                })?,
                () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider request canceled")),
            };
            if !response.status().is_success() {
                return Err(http_response_error(response).await);
            }
            let mut stream = response.bytes_stream();
            let mut decoder = SseDecoder::new();
            let mut events = Vec::new();
            while let Some(chunk) = tokio::select! { item = stream.next() => item, () = cancellation.cancelled() => return Err(ProviderError::new(ErrorCode::ProviderCanceled, "provider stream canceled")), }
            {
                let chunk = chunk.map_err(|error| {
                    let error = error.without_url();
                    ProviderError::new(
                        if error.is_timeout() {
                            ErrorCode::ProcessTimedOut
                        } else {
                            ErrorCode::ProviderProtocol
                        },
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

/// Source-compatible name retained for M2 fixtures.
pub type DeepSeekAdapter = OpenAiChatAdapter;

/// Identity a fragment gets when the stream never announced one.
const DEFAULT_TOOL_CALL_ID: &str = "tool-call";

/// Bounds one SSE stream may not exceed (M2-02).
///
/// A stream is untrusted input: without caps, a provider that never terminates a
/// frame or never stops announcing calls grows host memory without limit. The
/// defaults are generous for real answers and small enough to fail fast.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SseLimits {
    pub max_buffered_bytes: usize,
    pub max_frame_bytes: usize,
    pub max_tool_calls: usize,
    pub max_arguments_bytes: usize,
    /// Total decoded answer bytes (text plus tool arguments) one stream may
    /// produce. `max_buffered_bytes` bounds what is *unparsed*; this bounds what
    /// a stream that keeps producing small frames hands to the caller.
    pub max_output_bytes: usize,
}

impl Default for SseLimits {
    fn default() -> Self {
        Self {
            max_buffered_bytes: 4 * 1024 * 1024,
            max_frame_bytes: 1024 * 1024,
            max_tool_calls: 64,
            max_arguments_bytes: 1024 * 1024,
            max_output_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Default)]
pub struct SseDecoder {
    buffer: Vec<u8>,
    /// Call identity announced for each streamed `(choice, index)`.
    ///
    /// The fragment that opens a call carries `id` and `function.name`; every later
    /// fragment of the same call carries `index` and `function.arguments` only, so
    /// the pair is the only identity the whole stream agrees on. Choice is part of
    /// the key because an interleaved multi-choice stream reuses indexes per choice.
    tool_call_ids: BTreeMap<(u64, u64), String>,
    /// Call identities that arrived with an `id` but no `index`. They cannot be
    /// deduplicated against a slot, so they are remembered separately and counted
    /// against the same cap.
    id_only_ids: BTreeSet<String>,
    /// Distinct call identities seen so far. Every arm that creates one must
    /// count it here, or a stream can grow host memory with fresh identities.
    identity_count: usize,
    /// Running argument bytes per resolved call id, so a call cannot exceed
    /// `max_arguments_bytes` by splitting its arguments across fragments.
    arguments_bytes: BTreeMap<String, usize>,
    /// Decoded text plus argument bytes handed to the caller.
    decoded_bytes: usize,
    /// Call the most recent fragment belonged to, for a stream that omits `index`.
    last_tool_call_id: Option<String>,
    /// Whether a terminal marker (`[DONE]` or a `finish_reason`) was observed.
    saw_terminal: bool,
    limits: SseLimits,
}

impl SseDecoder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_limits(limits: SseLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Whether the stream reached a terminal marker before it ended.
    #[must_use]
    pub const fn saw_terminal(&self) -> bool {
        self.saw_terminal
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.limits.max_buffered_bytes {
            return Err(ProviderError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "provider SSE buffer exceeded {} bytes",
                    self.limits.max_buffered_bytes
                ),
            ));
        }
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
        // `consumed` is the end of the last parsed frame; `scanned` is how far a
        // delimiter search has already looked, so a feed that carries many small
        // frames is parsed in one pass instead of rescanning the tail per frame.
        let mut consumed = 0_usize;
        let mut scanned = 0_usize;
        loop {
            let mut found: Option<(usize, usize)> = None;
            let mut index = scanned.max(consumed);
            while index + 1 < self.buffer.len() {
                if self.buffer[index] == b'\n' && self.buffer[index + 1] == b'\n' {
                    found = Some((index, 2));
                    break;
                }
                if index + 3 < self.buffer.len() && self.buffer[index..index + 4] == *b"\r\n\r\n" {
                    found = Some((index, 4));
                    break;
                }
                index += 1;
            }
            let Some((index, width)) = found else {
                break;
            };
            let frame_len = index + width - consumed;
            if frame_len > self.limits.max_frame_bytes {
                return Err(ProviderError::new(
                    ErrorCode::FrameLimitExceeded,
                    format!(
                        "provider SSE frame exceeded {} bytes",
                        self.limits.max_frame_bytes
                    ),
                ));
            }
            let frame = self.buffer[consumed..index + width].to_vec();
            consumed = index + width;
            scanned = consumed;
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
                // The transport marker says the body ended; it does not overwrite a
                // finish reason the provider already named (a `tool_calls` turn ends
                // with both, and the reason is the provider's, not the marker's).
                if !self.saw_terminal {
                    self.saw_terminal = true;
                    output.push(ProviderStreamEvent::completed("stop"));
                }
            } else {
                output.extend(self.frame_events(&data)?);
            }
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
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
        let mut events = Vec::new();
        let choices = value
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if choices.is_empty() {
            // A usage-only frame carries no choice. Dropping it lost token
            // accounting silently, and refusing it turned a valid stream into an
            // error, so it becomes its own event (A06).
            if let Some(usage) = value.get("usage").and_then(Value::as_object) {
                let number = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
                return Ok(vec![ProviderStreamEvent::usage(
                    number("prompt_tokens"),
                    number("completion_tokens"),
                    number("total_tokens"),
                )]);
            }
            return Err(ProviderError::new(
                ErrorCode::ProviderProtocol,
                "provider SSE frame has no choice",
            ));
        }
        for (choice_index, choice) in choices.iter().enumerate() {
            let choice_index = u64::try_from(choice_index).unwrap_or(u64::MAX);
            let delta = choice.get("delta").cloned().unwrap_or_else(|| json!({}));
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                self.note_output(content.len())?;
                events.push(ProviderStreamEvent::text(content));
            }
            // `pi-ai`: endpoints stream reasoning as `reasoning_content` (DeepSeek,
            // llama.cpp), `reasoning` or `reasoning_text`; the first non-empty one is
            // the trace, so an endpoint that sends two copies is not doubled.
            if let Some(reasoning) = ["reasoning_content", "reasoning", "reasoning_text"]
                .iter()
                .find_map(|field| delta.get(*field).and_then(Value::as_str))
                .filter(|reasoning| !reasoning.is_empty())
            {
                self.note_output(reasoning.len())?;
                events.push(ProviderStreamEvent::thinking(reasoning));
            }
            if let Some(fragments) = delta.get("tool_calls").and_then(Value::as_array) {
                for fragment in fragments {
                    events.push(self.tool_call_fragment(choice_index, fragment)?);
                }
            }
            // A terminal frame can carry the last of the answer with it, so the
            // reason is reported in addition to that content: taking whichever
            // came first dropped `finish_reason` from every frame that also
            // carried prose or a fragment.
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.saw_terminal = true;
                events.push(ProviderStreamEvent::completed(reason));
            }
        }
        if events.is_empty() {
            events.push(ProviderStreamEvent::text(""));
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
    fn tool_call_fragment(
        &mut self,
        choice: u64,
        fragment: &Value,
    ) -> Result<ProviderStreamEvent, ProviderError> {
        let announced = fragment
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty());
        let index = fragment.get("index").and_then(Value::as_u64);
        let call_id = match (index, announced) {
            (Some(index), Some(id)) => {
                let key = (choice, index);
                match self.tool_call_ids.insert(key, id.to_owned()) {
                    // The same slot announcing a different id means the stream is
                    // not the paired transcript it claims to be; silently keeping
                    // the newer id turned one call into two identities.
                    Some(previous) if previous != id => {
                        return Err(ProviderError::new(
                            ErrorCode::ProviderProtocol,
                            format!("provider reused call slot {index} for {previous} and {id}"),
                        ));
                    }
                    Some(_) => {}
                    None => self.note_identity()?,
                }
                id.to_owned()
            }
            // A continuation fragment: only the index says which call it belongs to.
            (Some(index), None) => {
                let key = (choice, index);
                if !self.tool_call_ids.contains_key(&key) {
                    self.note_identity()?;
                }
                self.tool_call_ids
                    .entry(key)
                    .or_insert_with(|| format!("{DEFAULT_TOOL_CALL_ID}-{choice}-{index}"))
                    .clone()
            }
            (None, Some(id)) => {
                // A fragment that names its own call without a slot. Fresh ids are
                // as countable as slots, or a stream of one-fragment calls would
                // grow the assembled map without limit.
                if self.id_only_ids.insert(id.to_owned()) {
                    self.note_identity()?;
                }
                id.to_owned()
            }
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
        let arguments = function
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or("");
        // The cap is on one call's whole argument text, not one fragment's: a
        // stream that splits its arguments is still bounded by the same number.
        let total = self.arguments_bytes.entry(call_id.clone()).or_default();
        *total = total.saturating_add(arguments.len());
        if *total > self.limits.max_arguments_bytes {
            return Err(ProviderError::new(
                ErrorCode::OutputLimitExceeded,
                format!(
                    "provider tool arguments exceeded {} bytes",
                    self.limits.max_arguments_bytes
                ),
            ));
        }
        self.note_output(arguments.len())?;
        Ok(ProviderStreamEvent::tool_delta(
            call_id,
            function.get("name").and_then(Value::as_str).unwrap_or(""),
            arguments,
        ))
    }

    /// Count one new call identity against the stream's cap.
    fn note_identity(&mut self) -> Result<(), ProviderError> {
        self.identity_count = self.identity_count.saturating_add(1);
        if self.identity_count > self.limits.max_tool_calls {
            return Err(ProviderError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "provider stream announced more than {} tool calls",
                    self.limits.max_tool_calls
                ),
            ));
        }
        Ok(())
    }

    /// Count decoded answer bytes against the stream's cap.
    fn note_output(&mut self, bytes: usize) -> Result<(), ProviderError> {
        self.decoded_bytes = self.decoded_bytes.saturating_add(bytes);
        if self.decoded_bytes > self.limits.max_output_bytes {
            return Err(ProviderError::new(
                ErrorCode::OutputLimitExceeded,
                format!(
                    "provider stream output exceeded {} bytes",
                    self.limits.max_output_bytes
                ),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod sse_limit_tests {
    use super::{ErrorCode, SseDecoder, SseLimits};
    use serde_json::json;

    fn frame(choice_delta: &serde_json::Value) -> String {
        format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": choice_delta}]})
        )
    }

    #[test]
    fn a_stream_without_a_frame_terminator_is_bounded() {
        let limits = SseLimits {
            max_buffered_bytes: 64,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        let error = decoder
            .feed(&[b'x'; 128])
            .expect_err("an unterminated stream must not grow without limit");
        assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);
    }

    #[test]
    fn a_frame_larger_than_the_cap_is_typed() {
        let limits = SseLimits {
            max_frame_bytes: 32,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        let oversized = frame(&json!({"content": "x".repeat(64)}));
        let error = decoder
            .feed(oversized.as_bytes())
            .expect_err("oversized frames are refused");
        assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);
    }

    #[test]
    fn too_many_calls_and_too_many_argument_bytes_are_typed() {
        let limits = SseLimits {
            max_tool_calls: 1,
            max_arguments_bytes: 8,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "read_file", "arguments": "{}"}}]}))
                    .as_bytes(),
            )
            .expect("the first call fits");
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 1, "id": "call_b", "function": {"name": "read_file", "arguments": "{}"}}]}))
                    .as_bytes(),
            )
            .expect_err("the call cap is enforced");
        assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);

        let mut decoder = SseDecoder::with_limits(SseLimits {
            max_arguments_bytes: 8,
            ..SseLimits::default()
        });
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "read_file", "arguments": "0123456789"}}]}))
                    .as_bytes(),
            )
            .expect_err("argument bytes are capped");
        assert_eq!(error.code(), ErrorCode::OutputLimitExceeded);
    }

    #[test]
    fn a_slot_reannounced_with_another_id_is_refused() {
        let mut decoder = SseDecoder::new();
        decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"name": "read_file", "arguments": ""}}]}))
                    .as_bytes(),
            )
            .expect("the first announcement");
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "id": "call_b", "function": {"name": "read_file", "arguments": ""}}]}))
                    .as_bytes(),
            )
            .expect_err("one slot cannot change identity");
        assert_eq!(error.code(), ErrorCode::ProviderProtocol);
    }

    #[test]
    fn index_only_fragments_cannot_grow_the_call_map() {
        let limits = SseLimits {
            max_tool_calls: 1,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "function": {"arguments": "a"}}]}))
                    .as_bytes(),
            )
            .expect("the first index-only call fits");
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 1, "function": {"arguments": "b"}}]}))
                    .as_bytes(),
            )
            .expect_err("a fresh index is a fresh call and must be counted");
        assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);
    }

    #[test]
    fn id_only_fragments_are_counted_against_the_call_cap() {
        let limits = SseLimits {
            max_tool_calls: 1,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        decoder
            .feed(
                frame(&json!({"tool_calls": [{"id": "call_a", "function": {"arguments": "a"}}]}))
                    .as_bytes(),
            )
            .expect("the first id-only call fits");
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"id": "call_b", "function": {"arguments": "b"}}]}))
                    .as_bytes(),
            )
            .expect_err("a fresh id is a fresh call and must be counted");
        assert_eq!(error.code(), ErrorCode::FrameLimitExceeded);
    }

    #[test]
    fn split_arguments_are_capped_by_the_running_total() {
        let limits = SseLimits {
            max_arguments_bytes: 8,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "id": "call_a", "function": {"arguments": "01234"}}]}))
                    .as_bytes(),
            )
            .expect("the first fragment fits");
        let error = decoder
            .feed(
                frame(&json!({"tool_calls": [{"index": 0, "function": {"arguments": "56789"}}]}))
                    .as_bytes(),
            )
            .expect_err("the per-call total, not the fragment, is capped");
        assert_eq!(error.code(), ErrorCode::OutputLimitExceeded);
    }

    #[test]
    fn decoded_output_is_capped_across_frames() {
        let limits = SseLimits {
            max_output_bytes: 8,
            ..SseLimits::default()
        };
        let mut decoder = SseDecoder::with_limits(limits);
        decoder
            .feed(frame(&json!({"content": "12345"})).as_bytes())
            .expect("the first delta fits");
        let error = decoder
            .feed(frame(&json!({"content": "67890"})).as_bytes())
            .expect_err("many small frames still hit the output cap");
        assert_eq!(error.code(), ErrorCode::OutputLimitExceeded);
    }

    #[test]
    fn a_usage_only_frame_becomes_its_own_event_and_done_keeps_the_named_reason() {
        let mut decoder = SseDecoder::new();
        let events = decoder
            .feed(b"data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":4,\"total_tokens\":7}}\n\n")
            .expect("usage frame");
        assert_eq!(events, vec![super::ProviderStreamEvent::usage(3, 4, 7)]);

        let named = decoder
            .feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n")
            .expect("terminal frame");
        assert_eq!(
            named,
            vec![super::ProviderStreamEvent::completed("tool_calls")]
        );
        let done = decoder.feed(b"data: [DONE]\n\n").expect("transport marker");
        assert!(
            done.is_empty(),
            "the transport marker must not replace the provider's finish reason"
        );
        assert!(decoder.saw_terminal());
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
    use std::sync::Arc;

    use super::{
        CHAT_COMPLETIONS_PATH, DeepSeekAdapter, ErrorCode, ModelCapabilities,
        StaticCredentialResolver, chat_completions_endpoint,
    };

    fn adapter(endpoint: &str) -> Result<DeepSeekAdapter, super::ProviderError> {
        DeepSeekAdapter::new(
            endpoint,
            Arc::new(StaticCredentialResolver::new("fixture-secret")),
            ModelCapabilities::deepseek_fixture(),
        )
    }

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

    /// A remote cleartext endpoint would send the bearer token and the whole
    /// conversation in the clear, so it is refused at configuration time.
    #[test]
    fn cleartext_http_to_a_remote_host_is_refused() {
        let error = adapter("http://api.example.invalid/chat/completions")
            .err()
            .expect("remote http is refused");
        assert_eq!(error.code(), ErrorCode::ProviderProtocol);
    }

    /// Loopback is what local fixtures and loopback proxies use.
    #[test]
    fn cleartext_http_to_loopback_is_allowed_for_local_fixtures() {
        adapter("http://127.0.0.1:8080/chat/completions").expect("loopback http");
        adapter("http://localhost:8080/v1/chat/completions").expect("localhost http");
        adapter("http://[::1]:8080/chat/completions").expect("ipv6 loopback http");
    }

    #[test]
    fn https_and_unsupported_schemes_are_classified() {
        adapter("https://api.example.invalid/chat/completions").expect("https");
        let error = adapter("ftp://api.example.invalid/chat/completions")
            .err()
            .expect("ftp is refused");
        assert_eq!(error.code(), ErrorCode::ProviderProtocol);
    }
}

#[cfg(test)]
mod g03_openai_wire_snapshot_tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    use super::{
        MessageRole, ModelCapabilities, ModelProvider, OpenAiChatAdapter, ProviderMessage,
        ProviderRequest, StaticCredentialResolver,
    };
    use harness_types::RequestId;

    #[tokio::test]
    async fn g03_openai_chat_adapter_keeps_m2_wire_format() {
        let _fixture_lock = crate::LOOPBACK_FIXTURE_LOCK.lock().await;
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        let (sender, receiver) = std::sync::mpsc::channel();
        let fixture = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("adapter connection");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let mut header_end = None;
            let mut content_length = 0_usize;
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
                        .and_then(|value| value.trim().parse().ok())
                        .expect("content length");
                }
                if let Some(end) = header_end
                    && request.len() >= end + content_length
                {
                    break;
                }
            }
            let end = header_end.expect("request headers");
            sender
                .send(request[end..end + content_length].to_vec())
                .expect("snapshot delivery");
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .expect("fixture response");
        });

        let provider = OpenAiChatAdapter::with_options(
            format!("http://{address}/chat/completions"),
            Arc::new(StaticCredentialResolver::new("fixture-token")),
            ModelCapabilities {
                provider_id: "generic-openai".to_owned(),
                model: "fixture-model".to_owned(),
                supports_streaming: true,
                supports_tools: true,
                fixture: false,
            },
            super::OpenAiChatOptions {
                thinking: Some(super::Thinking {
                    level: super::ThinkingLevel::Off,
                    format: super::ThinkingFormat::DeepSeek,
                    model: None,
                }),
                headers: Vec::new(),
            },
        )
        .expect("generic OpenAI Chat adapter");
        let request = ProviderRequest::new(
            RequestId::generate(),
            "fixture-model",
            vec![ProviderMessage::new(MessageRole::User, "hello")],
        );
        let result = provider
            .stream(request, super::CancellationToken::new())
            .await;
        fixture.join().expect("fixture thread");
        let events = result.expect("fixture stream");
        assert!(!events.is_empty());
        let body = receiver.recv().expect("captured request body");
        assert_eq!(
            String::from_utf8(body).expect("UTF-8 body"),
            r#"{"messages":[{"content":"hello","role":"user"}],"model":"fixture-model","stream":true,"thinking":{"type":"disabled"}}"#
        );
    }
}

#[cfg(test)]
mod wire_tests {
    use super::{
        MessageRole, ProviderMessage, Reasoning, wire_messages, wire_messages_with_reasoning,
    };

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

    /// Measured 400: thinking mode demands `reasoning_content` back on turn two. As
    /// `pi-ai` does for `DeepSeek`, every assistant message carries it - the kept
    /// reasoning, or an empty one - and no other role does.
    #[test]
    fn a_thinking_endpoint_gets_every_assistant_reasoning_back() {
        let messages = vec![
            ProviderMessage::new(MessageRole::User, "list the files"),
            ProviderMessage::new(MessageRole::Assistant, "earlier answer"),
            ProviderMessage::new(MessageRole::Assistant, "calling a tool").with_reasoning(Some(
                Reasoning {
                    text: "look at src first".to_owned(),
                    signature: None,
                },
            )),
        ];
        let wire = wire_messages_with_reasoning(&messages);
        assert!(wire[0].get("reasoning_content").is_none());
        assert_eq!(wire[1]["reasoning_content"], "");
        assert_eq!(wire[2]["reasoning_content"], "look at src first");
        assert!(
            wire_messages(&messages)
                .iter()
                .all(|message| message.get("reasoning_content").is_none()),
            "an endpoint that does not ask for it never gets it"
        );
        // Reasoning is private: it is never serialized with the message.
        let stored = serde_json::to_value(&messages[2]).expect("serializable");
        assert!(
            !stored.to_string().contains("look at src first"),
            "{stored}"
        );
    }

    /// A tool result is sent as a user message, so the images a tool returned ride
    /// with it in the same block shape.
    #[test]
    fn a_tool_result_with_images_uses_content_blocks() {
        let image = super::ImageAttachment::inline("image/png", "iVBORw0KGgo=", "shot.png");
        let wire = ProviderMessage::tool_result("call-1", "loaded")
            .with_attachments(vec![image])
            .to_wire();
        assert_eq!(wire["role"], "user");
        let blocks = wire["content"].as_array().expect("blocks");
        assert!(
            blocks[0]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("[tool result]"))
        );
        assert_eq!(blocks[1]["type"], "image_url");
    }

    /// An image turns the message into the block shape the API reads images from.
    ///
    /// The docs are explicit: `content` is an array of blocks, the image is a
    /// `data:` URL the API sniffs by content, and images are accepted in a `user`
    /// message alone — any other role is refused, so the text shape is kept there.
    #[test]
    fn a_user_message_with_an_image_uses_content_blocks() {
        let image = super::ImageAttachment::inline(
            "image/png",
            "iVBORw0KGgo=",
            "shot.png (image/png, 1 KiB)",
        );
        let message = super::ProviderMessage::user_with_images("what is wrong here?", vec![image]);
        let wire = message.to_wire();
        assert_eq!(wire["role"], "user");
        let blocks = wire["content"].as_array().expect("content is an array");
        assert_eq!(blocks.len(), 2, "{wire}");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "what is wrong here?");
        assert_eq!(blocks[1]["type"], "image_url");
        assert_eq!(
            blocks[1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
        assert_eq!(blocks[1]["image_url"]["detail"], "auto");

        // No image: the shape every other message uses stays a plain string.
        let plain = super::ProviderMessage::new(super::MessageRole::User, "no image");
        assert_eq!(plain.to_wire()["content"], "no image");

        // A non-user role keeps the text shape rather than a request the API refuses.
        let mut assistant = super::ProviderMessage::new(super::MessageRole::Assistant, "hello");
        assistant.attachments.push(super::ImageAttachment::inline(
            "image/png",
            "iVBORw0KGgo=",
            "shot.png",
        ));
        assert_eq!(assistant.to_wire()["content"], "hello");
    }

    /// A link is sent as a link: the provider downloads it, this app never fetches it.
    #[test]
    fn a_remote_image_link_is_sent_as_the_link_itself() {
        let image =
            super::ImageAttachment::remote("https://example.com/shot.png", "shot.png (image url)")
                .expect("a public link is an attachment");
        let message = super::ProviderMessage::user_with_images("what is here?", vec![image]);
        let wire = message.to_wire();
        assert_eq!(
            wire["content"][1]["image_url"]["url"],
            "https://example.com/shot.png"
        );
        assert_eq!(wire["content"][1]["image_url"]["detail"], "auto");

        // Anything the API could not fetch is refused here, not at the provider.
        assert!(super::ImageAttachment::remote("ftp://example.com/shot.png", "x").is_none());
        assert!(super::ImageAttachment::remote("shot.png", "x").is_none());
        assert!(
            super::ImageAttachment::remote(
                format!(
                    "https://example.com/{}",
                    "a".repeat(super::MAX_REMOTE_URL_CHARS)
                ),
                "x"
            )
            .is_none(),
            "a link past the documented 8192 characters is refused"
        );
    }
}
