//! External tool and model-provider bridges.
//!
//! The tool bridge implements the P3 `ExternalToolDispatcher`, so an external
//! tool is reachable **only** through the existing gate: canonicalization,
//! policy, approval binding, durable intent and durable receipt all happen
//! before and after this code runs. This module cannot authorize anything.
//!
//! The provider bridge exposes a narrow model-provider capability whose stream
//! is normalized into the same event shapes the P2 runtime already consumes.

use std::sync::Arc;

use harness_providers::{
    CancellationToken, MessageRole, ModelCapabilities, ModelProvider, ProviderError,
    ProviderFuture, ProviderRequest, ProviderStreamEvent,
};
use harness_tools::{ExternalToolDispatcher, ToolOutput};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};

use crate::contracts::{ExtensionCapability, ExtensionError};
use crate::host::ExtensionRuntime;
use crate::transport::CallOutcome;

/// Bridges `CodingToolAction::ExternalTool` to a trusted extension process.
pub struct ExtensionToolDispatcher {
    runtime: Arc<ExtensionRuntime>,
}

impl std::fmt::Debug for ExtensionToolDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionToolDispatcher")
            .finish_non_exhaustive()
    }
}

impl ExtensionToolDispatcher {
    #[must_use]
    pub fn new(runtime: Arc<ExtensionRuntime>) -> Self {
        Self { runtime }
    }
}

impl ExternalToolDispatcher for ExtensionToolDispatcher {
    fn dispatch_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let Some(transport) = self.runtime.transport(plugin_id).await else {
                // An inactive or unloaded extension is a typed denial, never a
                // silent success.
                return Err(HarnessError::new(
                    ErrorCode::ExtensionNotFound,
                    format!("extension {plugin_id} is not active"),
                ));
            };
            if !transport.session().provides(ExtensionCapability::Tools) {
                return Err(HarnessError::new(
                    ErrorCode::ExtensionCapabilityMismatch,
                    format!("extension {plugin_id} did not negotiate the tools capability"),
                ));
            }
            let outcome = transport
                .call_with_deadline(
                    tool_name,
                    json!({"tool": tool_name, "arguments": arguments}),
                    timeout_ms,
                )
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            match outcome {
                CallOutcome::Answered { payload } => Ok(ToolOutput::ExternalTool {
                    plugin_id: plugin_id.to_owned(),
                    tool_name: tool_name.to_owned(),
                    payload,
                    inflight: transport.inflight(),
                }),
                CallOutcome::PluginError { code, message } => Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    format!("extension {plugin_id} refused {tool_name}: {code}: {message}"),
                )),
                CallOutcome::Canceled { reason } => Err(HarnessError::new(
                    ErrorCode::ProcessCanceled,
                    format!("extension {plugin_id} cancelled {tool_name}: {reason}"),
                )),
                CallOutcome::Uncertain { reason } => {
                    Err(HarnessError::new(ErrorCode::ProcessOutcomeUnknown, reason))
                }
            }
        })
    }
}

/// A narrow external model provider backed by a trusted extension process.
pub struct ExtensionProvider {
    runtime: Arc<ExtensionRuntime>,
    plugin_id: String,
}

impl std::fmt::Debug for ExtensionProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExtensionProvider")
            .field("plugin_id", &self.plugin_id)
            .finish_non_exhaustive()
    }
}

impl ExtensionProvider {
    pub fn new(
        runtime: Arc<ExtensionRuntime>,
        plugin_id: impl Into<String>,
    ) -> Result<Self, ExtensionError> {
        let plugin_id = plugin_id.into();
        if plugin_id.trim().is_empty() {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "an external provider requires a plugin id",
            ));
        }
        Ok(Self { runtime, plugin_id })
    }

    /// Normalize one plugin stream payload into the event shapes the P2 runtime
    /// already consumes. An unrecognized frame is a protocol error, never an
    /// invented event.
    pub fn normalize_stream(payload: &Value) -> Result<Vec<ProviderStreamEvent>, ProviderError> {
        let events = payload
            .get("events")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    "external provider payload has no events array",
                )
            })?;
        let mut normalized = Vec::new();
        for event in events {
            let kind = event.get("kind").and_then(Value::as_str).ok_or_else(|| {
                ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    "external provider event has no kind",
                )
            })?;
            let normalized_event = match kind {
                "started" => ProviderStreamEvent::started(),
                "text_delta" => {
                    let text = event.get("text").and_then(Value::as_str).ok_or_else(|| {
                        ProviderError::new(
                            ErrorCode::ProviderProtocol,
                            "a text_delta event requires text",
                        )
                    })?;
                    ProviderStreamEvent::text(text)
                }
                "completed" => ProviderStreamEvent::completed(
                    event
                        .get("stop_reason")
                        .and_then(Value::as_str)
                        .unwrap_or("stop"),
                ),
                other => {
                    return Err(ProviderError::new(
                        ErrorCode::ProviderProtocol,
                        format!("external provider emitted an unsupported event {other}"),
                    ));
                }
            };
            normalized.push(normalized_event);
        }
        Ok(normalized)
    }
}

impl ModelProvider for ExtensionProvider {
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            provider_id: format!("extension:{}", self.plugin_id),
            model: "fixture-model-1".to_owned(),
            supports_streaming: true,
            supports_tools: false,
            fixture: false,
        }
    }

    fn stream(&self, request: ProviderRequest, cancellation: CancellationToken) -> ProviderFuture {
        let runtime = Arc::clone(&self.runtime);
        let plugin_id = self.plugin_id.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ProviderError::new(
                    ErrorCode::ProviderCanceled,
                    "external provider call canceled before dispatch",
                ));
            }
            let transport = runtime.transport(&plugin_id).await.ok_or_else(|| {
                ProviderError::new(
                    ErrorCode::ServiceUnavailable,
                    format!("extension {plugin_id} is not active"),
                )
            })?;
            if !transport
                .session()
                .provides(ExtensionCapability::ModelProvider)
            {
                return Err(ProviderError::new(
                    ErrorCode::IncompatibleService,
                    format!(
                        "extension {plugin_id} did not negotiate the model provider capability"
                    ),
                ));
            }
            let prompt = request
                .messages
                .iter()
                .filter(|message| message.role != MessageRole::System)
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let outcome = transport
                .call(
                    "provider.stream",
                    json!({
                        "model": request.model,
                        "messages": prompt,
                        "temperature": request.temperature,
                    }),
                )
                .await
                .map_err(|error| ProviderError::new(error.code(), error.to_string()))?;
            match outcome {
                CallOutcome::Answered { payload } => Self::normalize_stream(&payload),
                CallOutcome::PluginError { code, message } => Err(ProviderError::new(
                    ErrorCode::ProviderProtocol,
                    format!("extension provider failed: {code}: {message}"),
                )),
                // A cancelled stream and an unsettled one are the same fact for
                // a provider caller: no usable answer arrived, and a retry is a
                // new attempt rather than a resumed one.
                CallOutcome::Canceled { reason } | CallOutcome::Uncertain { reason } => {
                    Err(ProviderError::new(ErrorCode::ProviderCanceled, reason))
                }
            }
        })
    }
}

/// Inspect a repository-provided extension or skill request without executing
/// anything or resolving a secret. This is the inert path that a repository
/// config may take.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigInspection {
    pub plugin_id: String,
    pub executable_digest: Option<harness_types::ContentHash>,
    pub requires_user_trust: bool,
    pub requested_capabilities: Vec<ExtensionCapability>,
    pub requested_secrets: Vec<String>,
    pub requested_host_methods: Vec<String>,
    pub notes: Vec<String>,
}

impl ConfigInspection {
    /// Inspect a parsed manifest. Reading this never starts a process and never
    /// resolves a secret reference.
    #[must_use]
    pub fn from_manifest(manifest: &crate::contracts::ExtensionManifest) -> Self {
        let mut notes = Vec::new();
        if !manifest.requested_secrets.is_empty() {
            notes.push(
                "this extension requests secret references; it cannot run until the user grants each one"
                    .to_owned(),
            );
        }
        if !manifest.requested_host_methods.is_empty() {
            notes.push(
                "requested host methods are checked against the allowlist and are not authority"
                    .to_owned(),
            );
        }
        Self {
            plugin_id: manifest.plugin_id.clone(),
            executable_digest: Some(manifest.executable_digest.clone()),
            requires_user_trust: true,
            requested_capabilities: manifest
                .provides
                .iter()
                .map(|offer| offer.capability)
                .collect(),
            requested_secrets: manifest.requested_secrets.clone(),
            requested_host_methods: manifest.requested_host_methods.clone(),
            notes,
        }
    }
}
