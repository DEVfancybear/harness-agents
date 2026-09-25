//! Lazy, per-chat-turn MCP connections and the shared external-tool adapters.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

use harness_extensions::{
    MCP_CALL_TIMEOUT_MS, McpClient, McpRequestCallbacks, McpRuntime, McpToolDispatcher,
};
use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError, McpServerConfigV2, ScopeId};
use serde_json::{Value, json};

use super::extensions::ActiveExtensions;

pub(super) type McpCallbackFactory =
    Arc<dyn Fn(&str) -> Arc<dyn McpRequestCallbacks> + Send + Sync>;

#[derive(Clone)]
struct AdvertisedTool {
    server: String,
    name: String,
    description: String,
    input_schema: Value,
    is_prompt: bool,
    timeout_ms: u64,
}

#[derive(Default)]
struct McpCatalog {
    advertised: BTreeMap<String, AdvertisedTool>,
}

impl McpCatalog {
    fn tool_count(&self) -> usize {
        self.advertised.len()
    }
}

impl ExternalToolCatalog for McpCatalog {
    fn schemas(&self) -> Vec<Value> {
        self.advertised
            .iter()
            .map(|(name, advertised)| {
                json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": format!("[MCP {}] {}", advertised.server, advertised.description),
                        "parameters": advertised.input_schema,
                    }
                })
            })
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        let advertised = self.advertised.get(name)?;
        Some(CodingToolAction::ExternalTool {
            plugin_id: format!(
                "{}::{}",
                if advertised.is_prompt {
                    "mcp_prompt"
                } else {
                    "mcp"
                },
                advertised.server
            ),
            tool_name: advertised.name.clone(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: advertised.timeout_ms,
        })
    }
}

/// A connected, bounded set of MCP servers owned by one interactive turn.
pub struct ActiveMcp {
    runtime: Arc<McpRuntime>,
    catalog: Arc<McpCatalog>,
    report: Vec<String>,
    configs: BTreeMap<String, McpServerConfigV2>,
    subscription_cancel: harness_providers::CancellationToken,
    subscription_tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl ActiveMcp {
    #[allow(clippy::too_many_lines)]
    pub async fn connect(
        configs: BTreeMap<String, McpServerConfigV2>,
        workspace: &Path,
        callback_factory: McpCallbackFactory,
        notice: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<Self, HarnessError> {
        let runtime = Arc::new(McpRuntime::new());
        let mut report = Vec::new();
        for (name, config) in &configs {
            if let Err(error) = config.validate(name) {
                return Err(HarnessError::new(error.code(), error.to_string()));
            }
            let scope = ScopeId::generate();
            let client = connect_one(name, config, workspace, scope, callback_factory(name)).await;
            match client {
                Ok(client) => {
                    runtime
                        .attach(name, client)
                        .await
                        .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
                }
                Err(error) if config.required => {
                    runtime.close_all().await;
                    return Err(HarnessError::new(
                        error.code(),
                        format!("required MCP server {name} could not start: {error}"),
                    ));
                }
                Err(error) => report.push(format!("{name}: unavailable ({error})")),
            }
        }
        let subscription_cancel = harness_providers::CancellationToken::new();
        let mut subscription_tasks = Vec::new();
        for name in runtime.attached_labels().await {
            let mut filter = harness_extensions::rmcp::model::SubscriptionFilter::builder()
                .tools_list_changed()
                .prompts_list_changed()
                .resources_list_changed();
            if let Some(cache) = runtime.cache(&name).await {
                filter = filter.resource_subscriptions(cache.resources().keys().cloned());
            }
            match runtime.listen(&name, filter.build()).await {
                Ok(mut subscription) if subscription_is_nonempty(subscription.acknowledged()) => {
                    report.push(format!("{name}: live MCP subscription active"));
                    let cancellation = subscription_cancel.clone();
                    let notice = Arc::clone(&notice);
                    let server = name.clone();
                    subscription_tasks.push(tokio::spawn(async move {
                        loop {
                            tokio::select! {
                                () = cancellation.cancelled() => {
                                    let _ = subscription.cancel().await;
                                    break;
                                }
                                result = subscription.next() => match result {
                                    Ok(Some(notification)) => {
                                        let summary = format!("MCP {server} notification: {notification:?}");
                                        notice(summary.chars().take(768).collect());
                                    }
                                    Ok(None) => break,
                                    Err(error) => {
                                        notice(format!("MCP {server} subscription ended: {error}"));
                                        break;
                                    }
                                }
                            }
                        }
                    }));
                }
                Ok(mut subscription) => {
                    let _ = subscription.cancel().await;
                }
                Err(error) => report.push(format!("{name}: subscriptions unavailable ({error})")),
            }
        }
        let caches = runtime.caches().await;
        let names = configs.keys().cloned().collect::<BTreeSet<_>>();
        let mut catalog = McpCatalog::default();
        for (name, config) in &configs {
            let Some(cache) = runtime.cache(name).await else {
                continue;
            };
            let enabled = config.enabled_tools.iter().collect::<BTreeSet<_>>();
            let disabled = config.disabled_tools.iter().collect::<BTreeSet<_>>();
            let timeout_ms = config
                .tool_timeout_seconds
                .unwrap_or(MCP_CALL_TIMEOUT_MS / 1000)
                .saturating_mul(1000)
                .min(120_000);
            for tool in cache.tools().values() {
                if (!enabled.is_empty() && !enabled.contains(&tool.name))
                    || disabled.contains(&tool.name)
                {
                    continue;
                }
                let tool_name = advertised_name(name, &tool.name);
                if catalog
                    .advertised
                    .insert(
                        tool_name.clone(),
                        AdvertisedTool {
                            server: name.clone(),
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            input_schema: tool.input_schema.clone(),
                            is_prompt: false,
                            timeout_ms,
                        },
                    )
                    .is_some()
                {
                    runtime.close_all().await;
                    return Err(HarnessError::new(
                        ErrorCode::DuplicateRegistration,
                        format!("MCP tool name collision at {tool_name}"),
                    ));
                }
            }
            match runtime.prompts(name).await {
                Ok(prompts) => {
                    for prompt in prompts {
                        let tool_name = prompt_advertised_name(name, &prompt.name);
                        let schema = prompt_schema(&prompt);
                        if catalog
                            .advertised
                            .insert(
                                tool_name.clone(),
                                AdvertisedTool {
                                    server: name.clone(),
                                    name: prompt.name,
                                    description: prompt
                                        .description
                                        .unwrap_or_else(|| "render this MCP prompt".to_owned()),
                                    input_schema: schema,
                                    is_prompt: true,
                                    timeout_ms,
                                },
                            )
                            .is_some()
                        {
                            runtime.close_all().await;
                            return Err(HarnessError::new(
                                ErrorCode::DuplicateRegistration,
                                format!("MCP prompt tool name collision at {tool_name}"),
                            ));
                        }
                    }
                }
                Err(error) => {
                    report.push(format!("{name}: prompt catalogue unavailable ({error})"));
                }
            }
            match runtime.resource_templates(name).await {
                Ok(templates) if !templates.is_empty() => {
                    for template in templates {
                        report.push(format!(
                            "{name}: resource template {} — {} (expand variables, then attach with @{}:<uri>)",
                            template.uri_template,
                            template.description.as_deref().unwrap_or("no description"),
                            name,
                        ));
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    report.push(format!("{name}: resource templates unavailable ({error})"));
                }
            }
        }
        if report.is_empty() {
            report.push(format!(
                "{} server(s), {} tool(s) available",
                caches.len(),
                catalog.tool_count()
            ));
        }
        for name in names {
            if !runtime.attached_labels().await.contains(&name) {
                report.push(format!("{name}: not connected"));
            }
        }
        Ok(Self {
            runtime,
            catalog: Arc::new(catalog),
            report,
            configs,
            subscription_cancel,
            subscription_tasks,
        })
    }

    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::clone(&self.catalog) as Arc<dyn ExternalToolCatalog>)
    }

    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(McpDispatcherMux {
            runtime: Arc::clone(&self.runtime),
            extensions: None,
        })
    }

    pub fn summary(&self) -> Vec<String> {
        let mut lines = self.report.clone();
        for (name, config) in &self.configs {
            let count = self
                .catalog
                .advertised
                .values()
                .filter(|tool| &tool.server == name)
                .count();
            lines.push(format!(
                "{name}: connected, {count} tool(s), transport={}",
                config.transport.as_deref().unwrap_or("stdio")
            ));
        }
        lines
    }

    pub async fn attach_mentions(
        &self,
        text: &str,
    ) -> (Vec<super::attachments::PreparedFile>, Vec<String>) {
        let mut files = Vec::new();
        let mut notes = Vec::new();
        for token in text.split_whitespace() {
            let candidate =
                token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | ';' | ')'));
            let Some(reference) = candidate.strip_prefix('@') else {
                continue;
            };
            let Some((server, uri)) = reference.split_once(':') else {
                continue;
            };
            if !self.configs.contains_key(server) || uri.is_empty() {
                continue;
            }
            match self.runtime.read_resource_explicit(server, uri).await {
                Ok(content) => files.push(super::attachments::PreparedFile {
                    label: format!(
                        "MCP {}:{} (text, {} bytes)",
                        server,
                        uri,
                        content.text.len()
                    ),
                    path: format!("mcp://{server}/{uri}"),
                    bytes: content.text.len(),
                    content: content.text,
                }),
                Err(error) => notes.push(format!(
                    "MCP resource {server}:{uri} not attached ({error})"
                )),
            }
        }
        (files, notes)
    }

    pub async fn shutdown(self) -> Vec<harness_extensions::mcp::McpUnloadReport> {
        self.subscription_cancel.cancel();
        for task in self.subscription_tasks {
            let _ = task.await;
        }
        self.runtime.drain_all().await
    }
}

/// One dispatcher route for both MCP and external extensions.
pub struct McpDispatcherMux {
    runtime: Arc<McpRuntime>,
    extensions: Option<Arc<dyn ExternalToolDispatcher>>,
}

impl ExternalToolDispatcher for McpDispatcherMux {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if let Some(server) = plugin_id.strip_prefix("mcp::") {
                McpToolDispatcher::new(Arc::clone(&self.runtime))
                    .validate_external(server, tool_name, arguments)
                    .await
            } else if let Some(server) = plugin_id.strip_prefix("mcp_prompt::") {
                self.runtime
                    .validate_prompt(server, tool_name, arguments)
                    .await
                    .map_err(|error| HarnessError::new(error.code(), error.to_string()))
            } else if let Some(extensions) = &self.extensions {
                extensions
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else {
                Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "external tool target is unavailable",
                ))
            }
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if let Some(server) = plugin_id.strip_prefix("mcp::") {
                McpToolDispatcher::new(Arc::clone(&self.runtime))
                    .dispatch_external(authorization, server, tool_name, arguments, timeout_ms)
                    .await
            } else if let Some(server) = plugin_id.strip_prefix("mcp_prompt::") {
                let values = arguments.as_object().ok_or_else(|| {
                    HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "MCP prompt arguments must be a JSON object",
                    )
                })?;
                let prompt_arguments = values
                    .iter()
                    .map(|(name, value)| {
                        value.as_str().map(|value| (name.clone(), value.to_owned()))
                    })
                    .collect::<Option<BTreeMap<_, _>>>()
                    .ok_or_else(|| {
                        HarnessError::new(
                            ErrorCode::InvalidPayload,
                            "MCP prompt arguments must be strings",
                        )
                    })?;
                let response = tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms.min(120_000)),
                    self.runtime.get_prompt(server, tool_name, prompt_arguments),
                )
                .await
                .map_err(|_| HarnessError::new(ErrorCode::ProcessTimedOut, "MCP prompt timed out"))?
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
                let payload = match response {
                    harness_extensions::rmcp::model::GetPromptResponse::Complete(result) => {
                        serde_json::to_value(result).map_err(|error| {
                            HarnessError::new(
                                ErrorCode::ExtensionProtocolError,
                                format!("MCP prompt response could not be encoded: {error}"),
                            )
                        })?
                    }
                    harness_extensions::rmcp::model::GetPromptResponse::InputRequired(_) => {
                        return Err(HarnessError::new(
                            ErrorCode::ExtensionProtocolUnsupported,
                            "MCP prompt requires an MRTR input round; this host supports single-round prompts",
                        ));
                    }
                    _ => {
                        return Err(HarnessError::new(
                            ErrorCode::ExtensionProtocolUnsupported,
                            "MCP prompt response kind is not supported",
                        ));
                    }
                };
                Ok(ToolOutput::ExternalTool {
                    plugin_id: plugin_id.to_owned(),
                    tool_name: tool_name.to_owned(),
                    payload,
                    inflight: 1,
                })
            } else if let Some(extensions) = &self.extensions {
                extensions
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else {
                Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "external tool target is unavailable",
                ))
            }
        })
    }
}

struct CatalogMux {
    mcp: Option<ExternalTools>,
    extensions: Option<ExternalTools>,
    delegate: Option<ExternalTools>,
    skills: Option<ExternalTools>,
    web: Option<ExternalTools>,
    goal: Option<ExternalTools>,
    repl: Option<ExternalTools>,
}

impl ExternalToolCatalog for CatalogMux {
    fn schemas(&self) -> Vec<Value> {
        self.mcp
            .iter()
            .chain(self.extensions.iter())
            .chain(self.delegate.iter())
            .chain(self.skills.iter())
            .chain(self.web.iter())
            .chain(self.goal.iter())
            .chain(self.repl.iter())
            .flat_map(ExternalTools::schemas)
            .collect()
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        self.mcp
            .as_ref()
            .and_then(|tools| tools.resolve(name, arguments))
            .or_else(|| {
                self.extensions
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
            .or_else(|| {
                self.delegate
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
            .or_else(|| {
                self.skills
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
            .or_else(|| {
                self.web
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
            .or_else(|| {
                self.goal
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
            .or_else(|| {
                self.repl
                    .as_ref()
                    .and_then(|tools| tools.resolve(name, arguments))
            })
    }
}

pub fn combined_tools_with_delegate(
    mcp: Option<&ActiveMcp>,
    extensions: Option<&ActiveExtensions>,
    delegate: Option<&super::delegation::DelegateHost>,
    skills: Option<&super::skills::SkillHost>,
    web: Option<&super::web::WebHost>,
    goal: Option<&super::goal::GoalHost>,
    repl: Option<&super::repl::ReplHost>,
) -> Option<ExternalTools> {
    if mcp.is_none()
        && extensions.is_none()
        && delegate.is_none()
        && skills.is_none()
        && web.is_none()
        && goal.is_none()
        && repl.is_none()
    {
        return None;
    }
    Some(ExternalTools::new(Arc::new(CatalogMux {
        mcp: mcp.map(ActiveMcp::tools),
        extensions: extensions.map(ActiveExtensions::tools),
        delegate: delegate.map(super::delegation::DelegateHost::tools),
        skills: skills.map(super::skills::SkillHost::tools),
        web: web.map(super::web::WebHost::tools),
        goal: goal.map(super::goal::GoalHost::tools),
        repl: repl.map(super::repl::ReplHost::tools),
    })))
}

pub fn combined_dispatcher(
    mcp: Option<&ActiveMcp>,
    extensions: Option<&ActiveExtensions>,
) -> Option<Arc<dyn ExternalToolDispatcher>> {
    match (mcp, extensions) {
        (Some(mcp), Some(extensions)) => Some(Arc::new(McpDispatcherMux {
            runtime: Arc::clone(&mcp.runtime),
            extensions: Some(extensions.dispatcher()),
        })),
        (Some(mcp), None) => Some(mcp.dispatcher()),
        (None, Some(extensions)) => Some(extensions.dispatcher()),
        (None, None) => None,
    }
}

pub fn combined_dispatcher_with_delegate(
    mcp: Option<&ActiveMcp>,
    extensions: Option<&ActiveExtensions>,
    delegate: Option<&super::delegation::DelegateHost>,
    skills: Option<&super::skills::SkillHost>,
    web: Option<&super::web::WebHost>,
    goal: Option<&super::goal::GoalHost>,
    repl: Option<&super::repl::ReplHost>,
) -> Option<Arc<dyn ExternalToolDispatcher>> {
    let inner = combined_dispatcher(mcp, extensions);
    if delegate.is_none() && skills.is_none() && web.is_none() && goal.is_none() && repl.is_none() {
        return inner;
    }
    Some(Arc::new(DelegateDispatcherMux {
        inner,
        delegate: delegate.map(super::delegation::DelegateHost::dispatcher),
        skills: skills.map(super::skills::SkillHost::dispatcher),
        web: web.map(super::web::WebHost::dispatcher),
        goal: goal.map(super::goal::GoalHost::dispatcher),
        repl: repl.map(super::repl::ReplHost::dispatcher),
    }))
}

struct DelegateDispatcherMux {
    inner: Option<Arc<dyn ExternalToolDispatcher>>,
    delegate: Option<Arc<dyn ExternalToolDispatcher>>,
    skills: Option<Arc<dyn ExternalToolDispatcher>>,
    web: Option<Arc<dyn ExternalToolDispatcher>>,
    goal: Option<Arc<dyn ExternalToolDispatcher>>,
    repl: Option<Arc<dyn ExternalToolDispatcher>>,
}

impl ExternalToolDispatcher for DelegateDispatcherMux {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            if plugin_id == "delegate" {
                self.delegate
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "delegate unavailable")
                    })?
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else if plugin_id == "skill" {
                self.skills
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "skill tools unavailable")
                    })?
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else if plugin_id == "web" {
                self.web
                    .as_ref()
                    .ok_or_else(|| HarnessError::new(ErrorCode::PolicyDenied, "web tools are off"))?
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else if plugin_id == "goal" {
                self.goal
                    .as_ref()
                    .ok_or_else(|| HarnessError::new(ErrorCode::PolicyDenied, "no goal is active"))?
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else if plugin_id == "repl" {
                self.repl
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "the Python REPL is off")
                    })?
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else if let Some(inner) = &self.inner {
                inner
                    .validate_external(plugin_id, tool_name, arguments)
                    .await
            } else {
                Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "external tool target is unavailable",
                ))
            }
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            if plugin_id == "delegate" {
                self.delegate
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "delegate unavailable")
                    })?
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else if plugin_id == "skill" {
                self.skills
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "skill tools unavailable")
                    })?
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else if plugin_id == "web" {
                self.web
                    .as_ref()
                    .ok_or_else(|| HarnessError::new(ErrorCode::PolicyDenied, "web tools are off"))?
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else if plugin_id == "goal" {
                self.goal
                    .as_ref()
                    .ok_or_else(|| HarnessError::new(ErrorCode::PolicyDenied, "no goal is active"))?
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else if plugin_id == "repl" {
                self.repl
                    .as_ref()
                    .ok_or_else(|| {
                        HarnessError::new(ErrorCode::PolicyDenied, "the Python REPL is off")
                    })?
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else if let Some(inner) = &self.inner {
                inner
                    .dispatch_external(authorization, plugin_id, tool_name, arguments, timeout_ms)
                    .await
            } else {
                Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "external tool target is unavailable",
                ))
            }
        })
    }
}

fn advertised_name(server: &str, tool: &str) -> String {
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("mcp__{}__{}", sanitize(server), sanitize(tool))
}

fn prompt_advertised_name(server: &str, prompt: &str) -> String {
    format!(
        "mcp__{}__prompt__{}",
        sanitize_name(server),
        sanitize_name(prompt)
    )
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn prompt_schema(prompt: &harness_extensions::rmcp::model::Prompt) -> Value {
    let arguments = prompt.arguments.as_deref().unwrap_or_default();
    let properties = arguments.iter().map(|argument| (
        argument.name.clone(),
        json!({ "type": "string", "description": argument.description.as_deref().unwrap_or("") }),
    )).collect::<serde_json::Map<_, _>>();
    let required = arguments
        .iter()
        .filter(|argument| argument.required == Some(true))
        .map(|argument| argument.name.clone())
        .collect::<Vec<_>>();
    json!({ "type": "object", "properties": properties, "required": required, "additionalProperties": false })
}

fn subscription_is_nonempty(filter: &harness_extensions::rmcp::model::SubscriptionFilter) -> bool {
    filter.tools_list_changed == Some(true)
        || filter.prompts_list_changed == Some(true)
        || filter.resources_list_changed == Some(true)
        || filter
            .resource_subscriptions
            .as_ref()
            .is_some_and(|resources| !resources.is_empty())
}

async fn connect_one(
    name: &str,
    config: &McpServerConfigV2,
    workspace: &Path,
    scope: ScopeId,
    callbacks: Arc<dyn McpRequestCallbacks>,
) -> Result<McpClient, harness_extensions::ExtensionError> {
    let generation = 1;
    match config.transport.as_deref().unwrap_or("stdio") {
        "stdio" => {
            let mut environment = BTreeMap::new();
            for (key, value) in &config.env {
                let resolved = if let Some(variable) = value.strip_prefix("secret://") {
                    std::env::var(variable).map_err(|_| {
                        harness_extensions::ExtensionError::new(
                            ErrorCode::PolicyDenied,
                            format!(
                                "MCP {name} requires the secret environment variable {variable}"
                            ),
                        )
                    })?
                } else {
                    value.clone()
                };
                environment.insert(key.clone(), resolved);
            }
            let command = config.command.as_deref().unwrap_or_default();
            let cwd = config.cwd.as_deref().map(|cwd| {
                let path = Path::new(cwd);
                if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    workspace.join(path)
                }
            });
            McpClient::connect_stdio_with_callbacks(
                command,
                config.args.clone(),
                environment,
                cwd,
                callbacks,
                scope,
                generation,
            )
            .await
        }
        "streamable_http" => {
            let token = config
                .bearer_token_env
                .as_deref()
                .map(|name| {
                    std::env::var(name).map_err(|_| {
                        harness_extensions::ExtensionError::new(
                            ErrorCode::PolicyDenied,
                            format!(
                                "MCP {name} requires the bearer-token environment variable {name}"
                            ),
                        )
                    })
                })
                .transpose()?;
            McpClient::connect_streamable_http_with_callbacks(
                config.url.as_deref().unwrap_or_default(),
                token.as_deref(),
                Some(callbacks),
                scope,
                generation,
            )
            .await
        }
        _ => Err(harness_extensions::ExtensionError::new(
            ErrorCode::ConfigParseError,
            "MCP transport is unsupported",
        )),
    }
}
