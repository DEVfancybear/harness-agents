//! MCP client adapter.
//!
//! MCP negotiation belongs to the official `rmcp` SDK contract and is never
//! assumed compatible with this repository's custom plugin protocol. The
//! adapter discovers tools and resources from a real MCP server process,
//! registers them in a scoped registry, and hands execution back to the P3
//! gate: an MCP-originated call is authorized exactly like any other tool call.
//!
//! Stdio and MCP Streamable HTTP are supported. HTTP requires TLS, permits
//! cleartext only on loopback, and reads bearer credentials from an environment
//! variable supplied by the host.
//!
//! Two rules this module exists to enforce:
//!
//! * **A method existing in the SDK is not a feature this build supports.**
//!   [`McpSupportMatrix`] is the single place that answers what is supported,
//!   and anything outside it is refused rather than quietly attempted.
//! * **Discovery is not permission.** A discovered tool is a name and a schema;
//!   the call still has to pass the tool gate, and this module never runs a
//!   server tool on its own initiative.

use std::{collections::BTreeMap, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use harness_kernel::{RegistrationToken, ScopedRegistry};
use harness_tools::{ExternalToolDispatcher, ToolOutput};
use harness_types::{ContentHash, ErrorCode, HarnessError, PluginInstanceId, ScopeId};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CancelTaskParams, ClientCapabilities,
        ClientConfig, GetPromptRequestParams, GetPromptResponse, GetTaskParams, Implementation,
        ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
        PaginatedRequestParams, Prompt, ReadResourceRequestParams, ResourceContents,
        ResourceTemplate, SubscriptionFilter, TaskPayload, TaskStatus, Tool,
    },
    service::{RequestContext, Subscription},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        streamable_http_client::StreamableHttpClientTransportConfig,
    },
};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tokio::time::timeout;

use crate::contracts::{CANCEL_GRACE_MS, ENVIRONMENT_ALLOWLIST, ExtensionError};
use crate::tasks::{
    RemoteTaskSnapshot, RemoteTaskState, SubmitFailure, TaskRemote, TaskSubmission,
};

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;

/// The MCP specification revision the pinned SDK implements.
///
/// Read from the SDK's own release documentation when this milestone was
/// implemented; the value is recorded here so a later SDK bump is a visible
/// decision rather than a silent drift.
pub const MCP_SPEC_REVISION: &str = "2026-07-28";

/// The pinned SDK version. Kept equal to the exact requirement in `Cargo.toml`.
pub const MCP_SDK_VERSION: &str = "3.4.0";

/// Bounded page walk for any `*/list` request. The adapter never loops forever
/// asking an untrusted server for more pages.
pub const MCP_MAX_PAGES: usize = 8;

/// Tools one server may advertise before its catalogue is refused as oversized.
pub const MCP_MAX_TOOLS: usize = 256;

/// Resources one server may advertise.
pub const MCP_MAX_RESOURCES: usize = 256;
pub const MCP_MAX_PROMPTS: usize = 256;
pub const MCP_MAX_RESOURCE_TEMPLATES: usize = 256;

/// Deadline for one discovery page walk.
pub const MCP_DISCOVERY_TIMEOUT_MS: u64 = 10_000;

/// Deadline for one resource read.
pub const MCP_READ_TIMEOUT_MS: u64 = 10_000;

/// Deadline for one tool invocation.
pub const MCP_CALL_TIMEOUT_MS: u64 = 30_000;

/// Bound on one resource body carried into the host.
pub const MCP_MAX_RESOURCE_BYTES: usize = 256 * 1024;

/// One MCP capability, named as the specification names it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum McpFeature {
    Tools,
    Resources,
    ResourceTemplates,
    Prompts,
    Sampling,
    Elicitation,
    Subscriptions,
    RemoteTransport,
    /// The SEP-2663 Tasks extension, `io.modelcontextprotocol/tasks`.
    Tasks,
}

impl McpFeature {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Resources => "resources",
            Self::ResourceTemplates => "resource_templates",
            Self::Prompts => "prompts",
            Self::Sampling => "sampling",
            Self::Elicitation => "elicitation",
            Self::Subscriptions => "subscriptions",
            Self::RemoteTransport => "remote_transport",
            Self::Tasks => "tasks",
        }
    }

    /// Whether this build implements the feature. Kept here as the one host
    /// capability inventory consumed by CLI output and feature guards.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(
            self,
            Self::Tools
                | Self::Resources
                | Self::ResourceTemplates
                | Self::Prompts
                | Self::Sampling
                | Self::Elicitation
                | Self::Subscriptions
                | Self::RemoteTransport
                | Self::Tasks
        )
    }

    /// Why an unsupported feature is not advertised. Empty when supported.
    #[must_use]
    pub const fn unsupported_reason(self) -> &'static str {
        ""
    }
}

/// What this build supports, and what it refuses to claim.
///
/// This is the single source of truth behind `ha extensions capabilities`, so a
/// feature can never be advertised by one code path and absent from another.
#[derive(Clone, Copy, Debug, Default)]
pub struct McpSupportMatrix;

impl McpSupportMatrix {
    /// Every feature the specification defines, supported or not.
    pub const ALL: [McpFeature; 9] = [
        McpFeature::Tools,
        McpFeature::Resources,
        McpFeature::ResourceTemplates,
        McpFeature::Prompts,
        McpFeature::Sampling,
        McpFeature::Elicitation,
        McpFeature::Subscriptions,
        McpFeature::RemoteTransport,
        McpFeature::Tasks,
    ];

    #[must_use]
    pub fn supported() -> Vec<McpFeature> {
        Self::ALL
            .into_iter()
            .filter(|feature| feature.is_supported())
            .collect()
    }

    #[must_use]
    pub fn unsupported() -> Vec<(McpFeature, &'static str)> {
        Self::ALL
            .into_iter()
            .filter(|feature| !feature.is_supported())
            .map(|feature| (feature, feature.unsupported_reason()))
            .collect()
    }

    /// Refuse an operation that belongs to an unsupported feature, so a caller
    /// gets a typed answer instead of a half-working one.
    pub fn require(feature: McpFeature) -> Result<(), ExtensionError> {
        if feature.is_supported() {
            return Ok(());
        }
        Err(ExtensionError::new(
            ErrorCode::ExtensionProtocolUnsupported,
            format!(
                "MCP feature {} is not supported: {}",
                feature.as_str(),
                feature.unsupported_reason()
            ),
        ))
    }

    #[must_use]
    pub const fn spec_revision() -> &'static str {
        MCP_SPEC_REVISION
    }

    #[must_use]
    pub const fn sdk_version() -> &'static str {
        MCP_SDK_VERSION
    }
}

/// One tool discovered from an MCP server.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    /// The JSON schema the server advertised, recorded so a change is visible.
    pub input_schema: Value,
    pub digest: ContentHash,
}

/// One resource discovered from an MCP server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResourceDescriptor {
    pub uri: String,
    pub name: String,
    pub description: String,
    pub mime_type: Option<String>,
    /// Digest of the advertised metadata, so a server that silently redefines a
    /// resource is visible without reading it.
    pub digest: ContentHash,
}

/// Where one resource body came from. A citation is only meaningful if the
/// server, its generation and the exact bytes are all named.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResourceProvenance {
    pub server: String,
    pub generation: u64,
    pub uri: String,
    pub digest: ContentHash,
}

/// One resource body, bounded and attributed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpResourceContent {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: String,
    pub digest: ContentHash,
    pub provenance: McpResourceProvenance,
}

/// The read-only metadata cache of one server generation.
///
/// It holds what discovery learned and nothing else. Cursors are deliberately
/// absent: the specification tells clients not to persist them, and a cached
/// cursor would let a later request resume a walk whose server state is gone.
#[derive(Clone, Debug, PartialEq)]
pub struct McpMetadataCache {
    server: String,
    generation: u64,
    tools: BTreeMap<String, McpToolDescriptor>,
    resources: BTreeMap<String, McpResourceDescriptor>,
    catalog_digest: ContentHash,
}

impl McpMetadataCache {
    /// Build a cache from one discovery pass.
    pub fn build(
        server: impl Into<String>,
        generation: u64,
        tools: Vec<McpToolDescriptor>,
        resources: Vec<McpResourceDescriptor>,
    ) -> Result<Self, ExtensionError> {
        if generation == 0 {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "an MCP metadata cache requires a positive generation",
            ));
        }
        let server = server.into();
        let tool_map = tools
            .into_iter()
            .map(|tool| (tool.name.clone(), tool))
            .collect::<BTreeMap<_, _>>();
        let resource_map = resources
            .into_iter()
            .map(|resource| (resource.uri.clone(), resource))
            .collect::<BTreeMap<_, _>>();
        let catalog_digest = ContentHash::from_canonical_json(&serde_json::json!({
            "domain": "mcp-catalog.v1",
            "server": server,
            "generation": generation,
            "tools": tool_map
                .values()
                .map(|tool| serde_json::json!({
                    "name": tool.name,
                    "digest": tool.digest.as_str(),
                }))
                .collect::<Vec<_>>(),
            "resources": resource_map
                .values()
                .map(|resource| serde_json::json!({
                    "uri": resource.uri,
                    "digest": resource.digest.as_str(),
                }))
                .collect::<Vec<_>>(),
        }))
        .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
        Ok(Self {
            server,
            generation,
            tools: tool_map,
            resources: resource_map,
            catalog_digest,
        })
    }

    #[must_use]
    pub fn server(&self) -> &str {
        &self.server
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn catalog_digest(&self) -> &ContentHash {
        &self.catalog_digest
    }

    /// Whether this cache still describes the live server generation.
    #[must_use]
    pub const fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }

    #[must_use]
    pub fn tools(&self) -> &BTreeMap<String, McpToolDescriptor> {
        &self.tools
    }

    #[must_use]
    pub fn resources(&self) -> &BTreeMap<String, McpResourceDescriptor> {
        &self.resources
    }

    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&McpToolDescriptor> {
        self.tools.get(name)
    }

    #[must_use]
    pub fn resource(&self, uri: &str) -> Option<&McpResourceDescriptor> {
        self.resources.get(uri)
    }
}

/// A live MCP client bound to one server process.
pub struct McpClient {
    scope_id: ScopeId,
    generation: u64,
    server_label: String,
    executable: PathBuf,
    /// The live SDK service, with the client identity this adapter negotiated.
    running: rmcp::service::RunningService<RoleClient, McpClientHandler>,
    tools: BTreeMap<String, McpToolDescriptor>,
    resources: BTreeMap<String, McpResourceDescriptor>,
    tokens: Vec<RegistrationToken>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("server", &self.server_label)
            .field("tools", &self.tools.len())
            .field("resources", &self.resources.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// The client identity and capabilities this adapter negotiates with.
///
/// The tasks extension is declared here rather than assumed: SEP-2663 lets a
/// server materialise a task **only** for a client that declared the extension,
/// so a client that stayed silent could never be handed a handle - and would see
/// a protocol error instead of an answer.
/// Application-provided MCP server callbacks. A caller must wire these to its
/// provider and interactive input surface before advertising sampling or
/// elicitation to a server.
#[allow(deprecated)]
pub trait McpRequestCallbacks: Send + Sync {
    #[allow(deprecated)]
    fn sample<'a>(
        &'a self,
        request: rmcp::model::CreateMessageRequestParams,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<rmcp::model::CreateMessageResult, rmcp::model::ErrorData>,
                > + Send
                + 'a,
        >,
    >;

    #[allow(deprecated)]
    fn elicit<'a>(
        &'a self,
        request: rmcp::model::ElicitRequestParams,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<rmcp::model::ElicitResult, rmcp::model::ErrorData>,
                > + Send
                + 'a,
        >,
    >;
}

struct McpClientHandler {
    info: ClientConfig,
    callbacks: Option<Arc<dyn McpRequestCallbacks>>,
}

impl McpClientHandler {
    fn new(callbacks: Option<Arc<dyn McpRequestCallbacks>>) -> Self {
        #[allow(deprecated)]
        let info = if callbacks.is_some() {
            ClientConfig::new(
                ClientCapabilities::builder()
                    .enable_tasks()
                    .enable_sampling()
                    .enable_elicitation()
                    .build(),
                Implementation::new("harness-agents", env!("CARGO_PKG_VERSION")),
            )
        } else {
            ClientConfig::new(
                ClientCapabilities::builder().enable_tasks().build(),
                Implementation::new("harness-agents", env!("CARGO_PKG_VERSION")),
            )
        };
        Self { info, callbacks }
    }
}

#[allow(deprecated)]
impl ClientHandler for McpClientHandler {
    fn get_info(&self) -> ClientConfig {
        self.info.clone()
    }

    async fn create_message(
        &self,
        request: rmcp::model::CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<rmcp::model::CreateMessageResult, rmcp::model::ErrorData> {
        match &self.callbacks {
            Some(callbacks) => callbacks.sample(request).await,
            None => Err(rmcp::model::ErrorData::method_not_found::<
                rmcp::model::CreateMessageRequestMethod,
            >()),
        }
    }

    async fn create_elicitation(
        &self,
        request: rmcp::model::ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<rmcp::model::ElicitResult, rmcp::model::ErrorData> {
        match &self.callbacks {
            Some(callbacks) => callbacks.elicit(request).await,
            None => Ok(rmcp::model::ElicitResult::new(
                rmcp::model::ElicitationAction::Decline,
            )),
        }
    }
}

impl McpClient {
    /// Start a local MCP server over stdio, complete the SDK handshake, and
    /// discover its tools and resources.
    ///
    /// Discovery is page-bounded: the adapter never loops forever asking for
    /// more pages from an untrusted server.
    ///
    /// The child receives the same minimal environment as a plugin transport —
    /// the allowlist only — so an MCP server never inherits the host's provider
    /// credentials. A host that wires repository or profile configuration to
    /// this entry point must pin the executable digest and require a trust
    /// grant first; this adapter itself performs no digest check.
    pub async fn connect_stdio(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        Self::connect_stdio_with_options(
            executable,
            arguments,
            BTreeMap::new(),
            None,
            scope_id,
            generation,
        )
        .await
    }

    /// Start a configured stdio server with a clean inherited environment.
    /// Only explicit safe host variables and the provided env map reach the
    /// child. The caller resolves secret references before calling this method.
    pub async fn connect_stdio_with_options(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        Self::connect_stdio_with_options_and_callbacks(
            executable,
            arguments,
            environment,
            cwd,
            None,
            scope_id,
            generation,
        )
        .await
    }

    /// Start a configured stdio server and expose sampling/elicitation only
    /// when the application has installed the corresponding callbacks.
    pub async fn connect_stdio_with_callbacks(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
        callbacks: Arc<dyn McpRequestCallbacks>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        Self::connect_stdio_with_options_and_callbacks(
            executable,
            arguments,
            environment,
            cwd,
            Some(callbacks),
            scope_id,
            generation,
        )
        .await
    }

    async fn connect_stdio_with_options_and_callbacks(
        executable: impl Into<PathBuf>,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
        callbacks: Option<Arc<dyn McpRequestCallbacks>>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        let executable = executable.into();
        let server_label = executable.file_stem().map_or_else(
            || "mcp-server".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let args = arguments.clone();
        let child_cwd = cwd.clone();
        let mut wrapped = CommandWrap::with_new(&executable, move |command| {
            // The host environment is not inherited wholesale: a provider API
            // key in the parent process must not reach an MCP server.
            command.env_clear();
            for name in ENVIRONMENT_ALLOWLIST {
                if let Ok(value) = std::env::var(name) {
                    command.env(name, value);
                }
            }
            for (name, value) in &environment {
                command.env(name, value);
            }
            if let Some(cwd) = &child_cwd {
                command.current_dir(cwd);
            }
            command
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
        });
        wrapped.wrap(KillOnDrop);
        #[cfg(windows)]
        wrapped.wrap(JobObject);
        #[cfg(unix)]
        wrapped.wrap(ProcessSession);
        let transport = TokioChildProcess::new(wrapped).map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("cannot start the MCP server: {error}"),
            )
        })?;
        let running = timeout(
            Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
            McpClientHandler::new(callbacks).serve(transport),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP handshake exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolUnsupported,
                format!("MCP handshake failed: {error}"),
            )
        })?;
        let mut client = Self {
            scope_id,
            generation,
            server_label,
            executable,
            running,
            tools: BTreeMap::new(),
            resources: BTreeMap::new(),
            tokens: Vec::new(),
        };
        client.discover().await?;
        Ok(client)
    }

    /// Connect to an MCP Streamable HTTP endpoint. Redirects are disabled by
    /// RMCP's reqwest client so an authorization header is never replayed to a
    /// different origin. Plain HTTP is accepted only for loopback fixtures.
    pub async fn connect_streamable_http(
        url: &str,
        bearer_token: Option<&str>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        Self::connect_streamable_http_with_callbacks(url, bearer_token, None, scope_id, generation)
            .await
    }

    /// Connect to Streamable HTTP with server-to-client callbacks enabled.
    pub async fn connect_streamable_http_with_callbacks(
        url: &str,
        bearer_token: Option<&str>,
        callbacks: Option<Arc<dyn McpRequestCallbacks>>,
        scope_id: ScopeId,
        generation: u64,
    ) -> Result<Self, ExtensionError> {
        let loopback = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| url.starts_with(prefix));
        if !(url.starts_with("https://") || loopback) {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                "MCP Streamable HTTP requires HTTPS; only loopback HTTP is allowed for fixtures",
            ));
        }
        let Some((_, authority_and_path)) = url.split_once("://") else {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "MCP endpoint URL is invalid",
            ));
        };
        let authority = authority_and_path
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        if authority.is_empty()
            || authority.contains('@')
            || authority.chars().any(char::is_whitespace)
        {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                "MCP endpoint URL must have a host and no embedded user information",
            ));
        }
        let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_owned());
        if let Some(token) = bearer_token.filter(|token| !token.is_empty()) {
            config = config.auth_header(token.to_owned());
        }
        let transport = StreamableHttpClientTransport::from_config(config);
        let running = timeout(
            Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
            McpClientHandler::new(callbacks).serve(transport),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP HTTP handshake exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolUnsupported,
                format!("MCP HTTP handshake failed: {error}"),
            )
        })?;
        let mut client = Self {
            scope_id,
            generation,
            server_label: authority.to_owned(),
            executable: PathBuf::from(url),
            running,
            tools: BTreeMap::new(),
            resources: BTreeMap::new(),
            tokens: Vec::new(),
        };
        client.discover().await?;
        Ok(client)
    }

    /// Discover tools and resources in one bounded pass.
    ///
    /// A malformed catalogue fails the whole discovery: a server that advertises
    /// a tool schema the host cannot validate is not partially trusted, because
    /// a half-registered catalogue is exactly how an unvalidated definition
    /// would reach the model.
    pub async fn discover(&mut self) -> Result<usize, ExtensionError> {
        let tools = self.discover_tools().await?;
        let resources = self.discover_resources().await?;
        Ok(tools + resources)
    }

    /// Discover tools with a bounded page walk.
    pub async fn discover_tools(&mut self) -> Result<usize, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Tools)?;
        let mut discovered: BTreeMap<String, McpToolDescriptor> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_PAGES {
            let params = cursor
                .clone()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let page = timeout(
                Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
                self.running.list_tools(params),
            )
            .await
            .map_err(|_| {
                ExtensionError::new(
                    ErrorCode::ProcessTimedOut,
                    format!("MCP list_tools exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
                )
            })?
            .map_err(|error| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP list_tools failed: {error}"),
                )
            })?;
            let ListToolsResult {
                tools, next_cursor, ..
            } = page;
            for tool in tools {
                let descriptor = describe(&tool)?;
                if discovered.len() >= MCP_MAX_TOOLS && !discovered.contains_key(&descriptor.name) {
                    return Err(ExtensionError::new(
                        ErrorCode::FrameLimitExceeded,
                        format!("MCP server advertises more than {MCP_MAX_TOOLS} tools"),
                    ));
                }
                discovered.insert(descriptor.name.clone(), descriptor);
            }
            match next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        self.tools = discovered;
        Ok(self.tools.len())
    }

    /// Discover resources with the same bounded page walk.
    ///
    /// The cursor is used inside this walk and then dropped: the specification
    /// forbids treating a cursor as durable, so nothing here stores one.
    pub async fn discover_resources(&mut self) -> Result<usize, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Resources)?;
        let mut discovered: BTreeMap<String, McpResourceDescriptor> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_PAGES {
            let params = cursor
                .clone()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let page = timeout(
                Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
                self.running.list_resources(params),
            )
            .await
            .map_err(|_| {
                ExtensionError::new(
                    ErrorCode::ProcessTimedOut,
                    format!("MCP list_resources exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
                )
            })?
            .map_err(|error| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP list_resources failed: {error}"),
                )
            })?;
            let ListResourcesResult {
                resources,
                next_cursor,
                ..
            } = page;
            for resource in resources {
                let descriptor = describe_resource(&resource)?;
                if discovered.len() >= MCP_MAX_RESOURCES
                    && !discovered.contains_key(&descriptor.uri)
                {
                    return Err(ExtensionError::new(
                        ErrorCode::FrameLimitExceeded,
                        format!("MCP server advertises more than {MCP_MAX_RESOURCES} resources"),
                    ));
                }
                discovered.insert(descriptor.uri.clone(), descriptor);
            }
            match next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        self.resources = discovered;
        Ok(self.resources.len())
    }

    /// List all prompts with an opaque, page-local cursor walk.
    pub async fn list_prompts(&self) -> Result<Vec<Prompt>, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Prompts)?;
        let mut prompts = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_PAGES {
            let params = cursor
                .clone()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let page: ListPromptsResult = timeout(
                Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
                self.running.list_prompts(params),
            )
            .await
            .map_err(|_| {
                ExtensionError::new(
                    ErrorCode::ProcessTimedOut,
                    format!("MCP list_prompts exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
                )
            })?
            .map_err(|error| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP list_prompts failed: {error}"),
                )
            })?;
            prompts.extend(page.prompts);
            if prompts.len() > MCP_MAX_PROMPTS {
                return Err(ExtensionError::new(
                    ErrorCode::FrameLimitExceeded,
                    format!("MCP server advertises more than {MCP_MAX_PROMPTS} prompts"),
                ));
            }
            cursor = page.next_cursor.filter(|next| !next.is_empty());
            if cursor.is_none() {
                return Ok(prompts);
            }
        }
        Err(ExtensionError::new(
            ErrorCode::FrameLimitExceeded,
            "MCP prompt catalogue exceeded the page limit",
        ))
    }

    /// Render one prompt from the remote server with the supplied arguments.
    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<GetPromptResponse, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Prompts)?;
        let request = GetPromptRequestParams::new(name.to_owned()).with_arguments(
            arguments
                .into_iter()
                .map(|(name, value)| (name, Value::String(value)))
                .collect::<Map<String, Value>>(),
        );
        timeout(
            Duration::from_millis(MCP_READ_TIMEOUT_MS),
            self.running.get_prompt_once(request),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP prompts/get {name} exceeded {MCP_READ_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP prompts/get {name} failed: {error}"),
            )
        })
    }

    /// List resource templates. Their RFC 6570 URI templates are returned as
    /// data; callers must expand variables explicitly before reading a resource.
    pub async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>, ExtensionError> {
        McpSupportMatrix::require(McpFeature::ResourceTemplates)?;
        let mut templates = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MCP_MAX_PAGES {
            let params = cursor
                .clone()
                .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor)));
            let page: ListResourceTemplatesResult = timeout(
                Duration::from_millis(MCP_DISCOVERY_TIMEOUT_MS),
                self.running.list_resource_templates(params),
            )
            .await
            .map_err(|_| {
                ExtensionError::new(
                    ErrorCode::ProcessTimedOut,
                    format!("MCP list_resource_templates exceeded {MCP_DISCOVERY_TIMEOUT_MS}ms"),
                )
            })?
            .map_err(|error| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP list_resource_templates failed: {error}"),
                )
            })?;
            templates.extend(page.resource_templates);
            if templates.len() > MCP_MAX_RESOURCE_TEMPLATES {
                return Err(ExtensionError::new(
                    ErrorCode::FrameLimitExceeded,
                    format!(
                        "MCP server advertises more than {MCP_MAX_RESOURCE_TEMPLATES} resource templates"
                    ),
                ));
            }
            cursor = page.next_cursor.filter(|next| !next.is_empty());
            if cursor.is_none() {
                return Ok(templates);
            }
        }
        Err(ExtensionError::new(
            ErrorCode::FrameLimitExceeded,
            "MCP resource-template catalogue exceeded the page limit",
        ))
    }

    /// Open a live notification subscription. The caller owns the returned
    /// handle and must cancel it or let it end before closing this client.
    pub async fn listen(&self, filter: SubscriptionFilter) -> Result<Subscription, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Subscriptions)?;
        self.running.peer().listen(filter).await.map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP subscriptions/listen failed: {error}"),
            )
        })
    }

    /// Read one discovered resource.
    ///
    /// Only text is carried: a binary body would need its own content pipeline
    /// and is refused rather than decoded into a string the host cannot verify.
    pub async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Resources)?;
        self.read_resource_explicit(uri).await
    }

    /// Read an exact URI named by the user. Resource templates and servers
    /// with dynamic URI spaces need not enumerate every readable URI; this
    /// entry point is therefore reserved for an explicit user attachment.
    pub async fn read_resource_explicit(
        &self,
        uri: &str,
    ) -> Result<McpResourceContent, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Resources)?;
        let result = timeout(
            Duration::from_millis(MCP_READ_TIMEOUT_MS),
            self.running
                .read_resource(ReadResourceRequestParams::new(uri.to_owned())),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP read_resource exceeded {MCP_READ_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP read_resource failed: {error}"),
            )
        })?;
        // A read must answer for exactly the resource that was asked for, in one
        // text part. A blob has no text form the host can attribute, and a
        // multi-part body has no single digest — both are refused rather than
        // lossily converted or silently truncated to the first part.
        let mut parts = Vec::new();
        for content in result.contents {
            match content {
                ResourceContents::TextResourceContents {
                    uri: body_uri,
                    mime_type,
                    text: body,
                    ..
                } => {
                    if body_uri != uri {
                        return Err(ExtensionError::new(
                            ErrorCode::ExtensionProtocolError,
                            format!("MCP server answered {uri} with contents for {body_uri}"),
                        ));
                    }
                    parts.push((body, mime_type));
                }
                _ => {
                    return Err(ExtensionError::new(
                        ErrorCode::BinaryContentDenied,
                        format!("MCP resource {uri} is not text"),
                    ));
                }
            }
        }
        let Ok([(body, mime_type)]) = <[(String, Option<String>); 1]>::try_from(parts) else {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP resource {uri} did not return exactly one text part"),
            ));
        };
        if body.len() > MCP_MAX_RESOURCE_BYTES {
            return Err(ExtensionError::new(
                ErrorCode::FrameLimitExceeded,
                format!(
                    "MCP resource {uri} is {} bytes, over the {MCP_MAX_RESOURCE_BYTES} byte limit",
                    body.len()
                ),
            ));
        }
        let digest = ContentHash::from_bytes(body.as_bytes());
        let provenance = McpResourceProvenance {
            server: self.server_label.clone(),
            generation: self.generation,
            uri: uri.to_owned(),
            digest: digest.clone(),
        };
        Ok(McpResourceContent {
            uri: uri.to_owned(),
            mime_type,
            text: body,
            digest,
            provenance,
        })
    }

    /// Register the discovered tools in a scoped registry. A duplicate name in
    /// the same layer is refused rather than overwritten.
    pub fn register_tools(
        &mut self,
        registry: &mut ScopedRegistry,
        instance_id: &PluginInstanceId,
    ) -> Result<Vec<String>, ExtensionError> {
        let mut registered = Vec::new();
        for name in self.tools.keys() {
            let token = registry
                .register(
                    &self.scope_id,
                    format!("mcp.tool:{name}"),
                    instance_id.clone(),
                    self.generation,
                )
                .map_err(|error| {
                    ExtensionError::new(
                        error.code(),
                        format!("cannot register MCP tool {name}: {error}"),
                    )
                })?;
            self.tokens.push(token);
            registered.push(name.clone());
        }
        Ok(registered)
    }

    /// Call one discovered tool.
    ///
    /// The arguments are validated against the advertised schema before the
    /// request leaves the host, and an unknown tool is refused locally rather
    /// than forwarded. This is the transport call: authority is decided by the
    /// tool gate that reaches it, never here.
    pub(crate) async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        self.call_tool_with_timeout(name, arguments, MCP_CALL_TIMEOUT_MS)
            .await
    }

    pub(crate) async fn call_tool_with_timeout(
        &self,
        name: &str,
        arguments: Value,
        timeout_ms: u64,
    ) -> Result<Value, ExtensionError> {
        self.validate_tool_arguments(name, &arguments)?;
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let timeout_ms = timeout_ms.clamp(1, 120_000);
        let response = timeout(
            Duration::from_millis(timeout_ms),
            self.running.call_tool_once(
                CallToolRequestParams::new(name.to_owned()).with_arguments(arguments),
            ),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP call_tool {name} exceeded {timeout_ms}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP call_tool failed: {error}"),
            )
        })?;
        let result = match response {
            CallToolResponse::Complete(result) => result,
            CallToolResponse::InputRequired(_) => {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolUnsupported,
                    "MCP input-required rounds are unsupported; the host sent the tool request once and will not retry it",
                ));
            }
            CallToolResponse::Task(_) => {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolUnsupported,
                    "MCP task responses are unsupported on the tool-dispatch path; reconcile the remote task explicitly",
                ));
            }
            _ => {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolUnsupported,
                    "the MCP server returned a tool response this host does not support",
                ));
            }
        };
        serde_json::to_value(&result).map_err(|_| {
            ExtensionError::new(
                ErrorCode::InvalidPayload,
                "MCP tool result is not serializable",
            )
        })
    }

    fn validate_tool_arguments(&self, name: &str, arguments: &Value) -> Result<(), ExtensionError> {
        let descriptor = self.tools.get(name).ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("MCP server did not advertise a tool named {name}"),
            )
        })?;
        validate_arguments(name, &descriptor.input_schema, arguments)?;
        Ok(())
    }

    /// Whether the server declared the tasks extension in the handshake.
    ///
    /// Read from the negotiated peer information, not from a claim in this
    /// build: a server that never offered the extension can never return a task
    /// handle, and a caller that assumed otherwise would wait for one.
    #[must_use]
    pub fn server_supports_tasks(&self) -> bool {
        self.running
            .peer_info()
            .is_some_and(|info| info.capabilities.supports_tasks())
    }

    /// Submit one tool call and report what the remote did with it.
    ///
    /// This is `call_tool_once`, not the SDK's `call_tool` helper: the helper
    /// drives MRTR rounds and deliberately refuses a task handle, while this
    /// path has to *see* one. The classification is the whole contract:
    ///
    /// - a JSON-RPC error is an **answer**, so nothing was applied;
    /// - a task result is a handle, so only polling is owed;
    /// - anything else - a timeout, a closed transport, an `input_required`
    ///   round this adapter does not drive - means the request may have landed,
    ///   and a second send could apply it twice.
    pub async fn submit_task(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<TaskSubmission, SubmitFailure> {
        let Some(descriptor) = self.tools.get(name) else {
            return Err(SubmitFailure::Definite(format!(
                "MCP server did not advertise a tool named {name}"
            )));
        };
        if let Err(error) = validate_arguments(name, &descriptor.input_schema, &arguments) {
            return Err(SubmitFailure::Definite(format!(
                "the arguments were refused before anything was sent: {error}"
            )));
        }
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        let call = timeout(
            Duration::from_millis(MCP_CALL_TIMEOUT_MS),
            self.running.call_tool_once(params),
        )
        .await;
        match call {
            Err(_) => Err(SubmitFailure::Ambiguous(format!(
                "the submit did not answer within {MCP_CALL_TIMEOUT_MS}ms; whether the remote applied it is unknown"
            ))),
            Ok(Err(error)) => Err(classify_send_failure(name, &error)),
            Ok(Ok(CallToolResponse::Complete(result))) => {
                let value = serde_json::to_value(&result).map_err(|_| {
                    SubmitFailure::Ambiguous(
                        "the remote completed the work inside the call but its result could not be read; the effect is applied and unknown to this host"
                            .to_owned(),
                    )
                })?;
                Ok(TaskSubmission::Completed { result: value })
            }
            Ok(Ok(CallToolResponse::Task(created))) => Ok(TaskSubmission::Accepted {
                remote_task_id: created.task.task_id.clone(),
                // The seed status of a task that was just materialised is
                // `working`; an unknown one is treated the same way, because it
                // is certainly not a terminal state this host may settle on.
                state: state_of(created.task.status).unwrap_or(RemoteTaskState::Working),
                poll_interval_ms: created.task.poll_interval_ms,
            }),
            Ok(Ok(CallToolResponse::InputRequired(_))) => Err(SubmitFailure::Ambiguous(
                "the remote asked for input mid-call instead of answering; the call is unfinished"
                    .to_owned(),
            )),
            // The response enum is non-exhaustive. An answer this build cannot
            // interpret is an answer to a mutation it cannot interpret, which is
            // the ambiguous case by definition.
            Ok(Ok(_)) => Err(SubmitFailure::Ambiguous(
                "the remote answered with a response this build does not understand".to_owned(),
            )),
        }
    }

    /// Read one remote task's current state.
    ///
    /// # Errors
    /// Fails when the remote refuses or the reply carries a status this build
    /// does not know.
    pub async fn task_status(
        &self,
        remote_task_id: &str,
    ) -> Result<RemoteTaskSnapshot, ExtensionError> {
        let result = timeout(
            Duration::from_millis(MCP_CALL_TIMEOUT_MS),
            self.running.get_task(GetTaskParams::new(remote_task_id)),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP tasks/get {remote_task_id} exceeded {MCP_CALL_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP tasks/get failed: {error}"),
            )
        })?;
        let task = &result.task.task;
        let Some(status) = state_of(task.status) else {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                "the remote reported a task status this build does not know",
            ));
        };
        let mut snapshot = RemoteTaskSnapshot::new(task.task_id.clone(), status);
        snapshot.status_message.clone_from(&task.status_message);
        snapshot.poll_interval_ms = task.poll_interval_ms;
        match &result.task.payload {
            TaskPayload::Completed { result } => {
                snapshot.result = Some(Value::Object(result.clone()));
            }
            TaskPayload::Failed { error } => {
                snapshot.error = Some(Value::Object(error.clone()));
            }
            _ => {}
        }
        Ok(snapshot)
    }

    /// Ask the remote to cancel a task. Cooperative: this reports delivery, and
    /// the task's own next status is what says how the work ended.
    ///
    /// # Errors
    /// Fails when the remote refuses the request.
    pub async fn cancel_remote_task(&self, remote_task_id: &str) -> Result<(), ExtensionError> {
        timeout(
            Duration::from_millis(MCP_CALL_TIMEOUT_MS),
            self.running
                .cancel_task(CancelTaskParams::new(remote_task_id)),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP tasks/cancel {remote_task_id} exceeded {MCP_CALL_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP tasks/cancel failed: {error}"),
            )
        })
    }

    /// Whether the server advertised a tool schema that differs from a pinned
    /// digest. A change requires renegotiation before the tool is used.
    #[must_use]
    pub fn schema_drift(&self, pinned: &BTreeMap<String, ContentHash>) -> Vec<String> {
        let mut drifted = Vec::new();
        for (name, descriptor) in &self.tools {
            match pinned.get(name) {
                Some(expected) if expected != &descriptor.digest => drifted.push(name.clone()),
                None => drifted.push(name.clone()),
                _ => {}
            }
        }
        drifted
    }

    /// Snapshot the discovered metadata as a read-only cache.
    pub fn metadata_cache(&self) -> Result<McpMetadataCache, ExtensionError> {
        McpMetadataCache::build(
            self.server_label.clone(),
            self.generation,
            self.tools.values().cloned().collect(),
            self.resources.values().cloned().collect(),
        )
    }

    #[must_use]
    pub fn tools(&self) -> &BTreeMap<String, McpToolDescriptor> {
        &self.tools
    }

    #[must_use]
    pub fn resources(&self) -> &BTreeMap<String, McpResourceDescriptor> {
        &self.resources
    }

    #[must_use]
    pub fn server_label(&self) -> &str {
        &self.server_label
    }

    #[must_use]
    pub fn executable(&self) -> &PathBuf {
        &self.executable
    }

    #[must_use]
    pub fn scope_id(&self) -> &ScopeId {
        &self.scope_id
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Stop the client and its server process.
    pub async fn close(self) {
        let _ = self.running.cancel().await;
    }
}

/// Map the SDK's task status onto this host's vocabulary.
///
/// `None` means a status this build does not know, which the SDK allows because
/// its enum is non-exhaustive. A reader refuses it rather than guessing; a
/// submit only ever sees it on the seed state of a task that was just created.
fn state_of(status: TaskStatus) -> Option<RemoteTaskState> {
    match status {
        TaskStatus::Working => Some(RemoteTaskState::Working),
        TaskStatus::InputRequired => Some(RemoteTaskState::InputRequired),
        TaskStatus::Completed => Some(RemoteTaskState::Completed),
        TaskStatus::Failed => Some(RemoteTaskState::Failed),
        TaskStatus::Cancelled => Some(RemoteTaskState::Cancelled),
        _ => None,
    }
}

/// Classify a failed call: only a pre-send refusal is definite.
///
/// A JSON-RPC error is **not** proof that nothing happened. The pinned SDK's own
/// SEP-2663 guard proves the point: a server that materialises a task for a
/// client which did not declare the extension has already created and persisted
/// that task by the time the guard replaces the response with an error. A remote
/// error is therefore an answer about the *answer*, and the safe reading is the
/// one that never sends the request again.
fn classify_send_failure(operation: &str, error: &rmcp::ServiceError) -> SubmitFailure {
    match error {
        rmcp::ServiceError::McpError(data) => SubmitFailure::Ambiguous(format!(
            "the remote refused {operation} after the request was sent ({}: {}); whether it applied the work first is unknown",
            data.code.0, data.message
        )),
        other => SubmitFailure::Ambiguous(format!(
            "the submit of {operation} did not get an answer: {other}"
        )),
    }
}

/// A [`TaskRemote`] over one MCP server's task-returning tool.
///
/// The MCP mapping, pinned to the SDK's SEP-2663 support: `submit` is the tool
/// call that may answer with a task handle, `status` is `tasks/get`, `cancel` is
/// `tasks/cancel` and is cooperative.
pub struct McpTaskRemote {
    client: Arc<McpClient>,
    server_id: String,
    tool: String,
}

impl McpTaskRemote {
    #[must_use]
    pub fn new(
        client: Arc<McpClient>,
        server_id: impl Into<String>,
        tool: impl Into<String>,
    ) -> Self {
        Self {
            client,
            server_id: server_id.into(),
            tool: tool.into(),
        }
    }

    #[must_use]
    pub fn client(&self) -> &Arc<McpClient> {
        &self.client
    }
}

impl TaskRemote for McpTaskRemote {
    fn server_id(&self) -> &str {
        &self.server_id
    }

    fn submit<'a>(
        &'a self,
        operation: &'a str,
        request: &'a Value,
    ) -> crate::tasks::RemoteFuture<'a, Result<TaskSubmission, SubmitFailure>> {
        Box::pin(async move {
            // A job names the operation it submitted, and the transport owns
            // which tool implements it. A mismatch is refused *before* anything
            // is sent, which is why it is a definite failure.
            if operation != self.tool {
                return Err(SubmitFailure::Definite(format!(
                    "this transport serves {} and not {operation}",
                    self.tool
                )));
            }
            self.client.submit_task(&self.tool, request.clone()).await
        })
    }

    fn status<'a>(
        &'a self,
        remote_task_id: &'a str,
    ) -> crate::tasks::RemoteFuture<'a, Result<RemoteTaskSnapshot, ExtensionError>> {
        Box::pin(async move { self.client.task_status(remote_task_id).await })
    }

    fn cancel<'a>(
        &'a self,
        remote_task_id: &'a str,
    ) -> crate::tasks::RemoteFuture<'a, Result<(), ExtensionError>> {
        Box::pin(async move { self.client.cancel_remote_task(remote_task_id).await })
    }
}

/// Validate one advertised tool schema.
///
/// A tool whose schema the host cannot understand is refused at discovery time,
/// which is the only moment where refusing it is free.
pub fn validate_tool_schema(descriptor: &McpToolDescriptor) -> Result<(), ExtensionError> {
    validate_supported_object_schema(&descriptor.name, &descriptor.input_schema)
}

fn validate_supported_object_schema(tool: &str, schema: &Value) -> Result<(), ExtensionError> {
    let object = schema.as_object().ok_or_else(|| {
        ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            format!("MCP tool {tool} advertised a schema that is not an object"),
        )
    })?;
    validate_schema_keys(
        tool,
        object,
        &[
            "type",
            "properties",
            "required",
            "additionalProperties",
            "title",
            "description",
            "default",
            "examples",
            "deprecated",
            "readOnly",
            "writeOnly",
            "$schema",
            "$id",
            "$comment",
        ],
    )?;
    validate_object_schema_type(tool, object)?;
    validate_object_properties(tool, object)?;
    validate_required_properties(tool, object)?;
    validate_additional_properties(tool, object)?;
    Ok(())
}

fn validate_object_schema_type(
    tool: &str,
    object: &Map<String, Value>,
) -> Result<(), ExtensionError> {
    match object.get("type") {
        Some(Value::String(kind)) if kind == "object" => {}
        Some(Value::String(kind)) => {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!(
                    "MCP tool {tool} advertised schema type {kind}; only object arguments are supported"
                ),
            ));
        }
        Some(_) => {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP tool {tool} advertised a non-string schema type"),
            ));
        }
        // A schema without `type` is accepted only when it declares properties:
        // a bare `{}` accepts anything and would defeat argument validation.
        None => {
            if !object.contains_key("properties") {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!(
                        "MCP tool {tool} advertised a schema with neither a type nor properties"
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_object_properties(
    tool: &str,
    object: &Map<String, Value>,
) -> Result<(), ExtensionError> {
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        return Err(ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            format!("MCP tool {tool} advertised non-object properties"),
        ));
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        for (name, property_schema) in properties {
            validate_property_schema(tool, name, property_schema)?;
        }
    }
    Ok(())
}

fn validate_required_properties(
    tool: &str,
    object: &Map<String, Value>,
) -> Result<(), ExtensionError> {
    let declared = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(required) = object.get("required") {
        let Some(names) = required.as_array() else {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP tool {tool} advertised a non-array required list"),
            ));
        };
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            let Some(name) = name.as_str() else {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP tool {tool} required a non-string property"),
                ));
            };
            if !seen.insert(name) {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP tool {tool} repeated required property {name}"),
                ));
            }
            if !declared.iter().any(|declared| declared == name) {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP tool {tool} requires {name}, which it does not declare"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_additional_properties(
    tool: &str,
    object: &Map<String, Value>,
) -> Result<(), ExtensionError> {
    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        return Err(unsupported_schema(
            tool,
            "additionalProperties must be a boolean",
        ));
    }
    Ok(())
}

fn validate_schema_keys(
    tool: &str,
    schema: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), ExtensionError> {
    if let Some(keyword) = schema.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(unsupported_schema(
            tool,
            &format!("schema keyword {keyword:?} is not supported"),
        ));
    }
    Ok(())
}

fn validate_property_schema(tool: &str, name: &str, schema: &Value) -> Result<(), ExtensionError> {
    let Some(schema) = schema.as_object() else {
        return Err(unsupported_schema(
            tool,
            &format!("property {name:?} does not have an object schema"),
        ));
    };
    validate_schema_keys(
        tool,
        schema,
        &[
            "type",
            "title",
            "description",
            "default",
            "examples",
            "deprecated",
            "readOnly",
            "writeOnly",
        ],
    )?;
    if let Some(kind) = schema.get("type") {
        let Some(kind) = kind.as_str() else {
            return Err(unsupported_schema(
                tool,
                &format!("property {name:?} has a non-string type"),
            ));
        };
        if !matches!(
            kind,
            "string" | "number" | "integer" | "boolean" | "object" | "array" | "null"
        ) {
            return Err(unsupported_schema(
                tool,
                &format!("property {name:?} has unsupported type {kind:?}"),
            ));
        }
    }
    Ok(())
}

fn unsupported_schema(tool: &str, detail: &str) -> ExtensionError {
    ExtensionError::new(
        ErrorCode::ExtensionProtocolError,
        format!("MCP tool {tool} advertised a schema the host cannot validate: {detail}"),
    )
}

/// Validate call arguments against an advertised schema.
///
/// This is structural validation, not a general JSON Schema implementation: it
/// covers the required, unknown-property and primitive-type cases the adapter
/// depends on, and refuses anything it cannot check rather than assuming it is
/// fine.
pub fn validate_arguments(
    tool: &str,
    schema: &Value,
    arguments: &Value,
) -> Result<(), ExtensionError> {
    validate_supported_object_schema(tool, schema)?;
    let Some(arguments) = arguments.as_object() else {
        return Err(ExtensionError::new(
            ErrorCode::InvalidPayload,
            format!("arguments for MCP tool {tool} must be a JSON object"),
        ));
    };
    let Some(schema) = schema.as_object() else {
        return Err(ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            format!("MCP tool {tool} has no object schema to validate against"),
        ));
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !arguments.contains_key(name) {
                return Err(ExtensionError::new(
                    ErrorCode::InvalidPayload,
                    format!("MCP tool {tool} requires the argument {name}"),
                ));
            }
        }
    }
    // `additionalProperties` defaults to `true` in JSON Schema, so an undeclared
    // argument is refused only when the schema explicitly closes the object.
    let closed = matches!(
        schema.get("additionalProperties").and_then(Value::as_bool),
        Some(false)
    );
    for (name, value) in arguments {
        let Some(declared) = properties.and_then(|properties| properties.get(name)) else {
            if closed {
                return Err(ExtensionError::new(
                    ErrorCode::InvalidPayload,
                    format!("MCP tool {tool} does not declare the argument {name}"),
                ));
            }
            continue;
        };
        if let Some(kind) = declared.get("type").and_then(Value::as_str)
            && !value_matches(kind, value)
        {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                format!("MCP tool {tool} argument {name} is not a {kind}"),
            ));
        }
    }
    Ok(())
}

fn value_matches(kind: &str, value: &Value) -> bool {
    match kind {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        _ => false,
    }
}

/// Connected MCP servers, keyed by the label the host uses for them.
///
/// Every invocation the host performs goes through this registry, which is what
/// makes "the adapter is the only path to a server" checkable rather than
/// aspirational.
#[derive(Default)]
pub struct McpRuntime {
    servers: Mutex<BTreeMap<String, Arc<McpClient>>>,
}

/// Outcome of draining one configured MCP connection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpUnloadReport {
    pub label: String,
    pub drained: bool,
    pub inflight: usize,
}

impl std::fmt::Debug for McpRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("McpRuntime").finish_non_exhaustive()
    }
}

impl McpRuntime {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a connected server. A duplicate label is refused rather than
    /// silently replacing a live server.
    pub async fn attach(&self, label: &str, client: McpClient) -> Result<(), ExtensionError> {
        let mut servers = self.servers.lock().await;
        if servers.contains_key(label) {
            return Err(ExtensionError::new(
                ErrorCode::DuplicateRegistration,
                format!("an MCP server is already attached as {label}"),
            ));
        }
        servers.insert(label.to_owned(), Arc::new(client));
        Ok(())
    }

    async fn client(&self, label: &str) -> Option<Arc<McpClient>> {
        self.servers.lock().await.get(label).cloned()
    }

    /// List remote prompt descriptors for an attached server.
    pub async fn prompts(&self, label: &str) -> Result<Vec<Prompt>, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.list_prompts().await
    }

    /// Render a remote prompt. Argument validation and response handling stay
    /// inside the pinned MCP client adapter.
    pub async fn get_prompt(
        &self,
        label: &str,
        prompt: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<GetPromptResponse, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.get_prompt(prompt, arguments).await
    }

    /// Validate that a named prompt exists and its required arguments are
    /// supplied as strings before the external-tool receipt is opened.
    pub async fn validate_prompt(
        &self,
        label: &str,
        prompt_name: &str,
        arguments: &Value,
    ) -> Result<(), ExtensionError> {
        let prompts = self.prompts(label).await?;
        let prompt = prompts
            .iter()
            .find(|prompt| prompt.name == prompt_name)
            .ok_or_else(|| {
                ExtensionError::new(
                    ErrorCode::ExtensionNotFound,
                    format!("MCP prompt {prompt_name} is not advertised by {label}"),
                )
            })?;
        let values = arguments.as_object().ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::InvalidPayload,
                "MCP prompt arguments must be a JSON object",
            )
        })?;
        if let Some(definitions) = &prompt.arguments {
            for definition in definitions {
                let value = values.get(&definition.name);
                if definition.required == Some(true) && value.is_none() {
                    return Err(ExtensionError::new(
                        ErrorCode::InvalidPayload,
                        format!(
                            "required MCP prompt argument {:?} is missing",
                            definition.name
                        ),
                    ));
                }
                if value.is_some_and(|value| !value.is_string()) {
                    return Err(ExtensionError::new(
                        ErrorCode::InvalidPayload,
                        format!("MCP prompt argument {:?} must be a string", definition.name),
                    ));
                }
            }
        }
        if values.iter().any(|(name, value)| {
            !value.is_string()
                || prompt.arguments.as_ref().is_some_and(|definitions| {
                    !definitions
                        .iter()
                        .any(|definition| &definition.name == name)
                })
        }) {
            return Err(ExtensionError::new(
                ErrorCode::InvalidPayload,
                "MCP prompt arguments contain an unknown or non-string value",
            ));
        }
        Ok(())
    }

    /// List remote resource templates for an attached server.
    pub async fn resource_templates(
        &self,
        label: &str,
    ) -> Result<Vec<ResourceTemplate>, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.list_resource_templates().await
    }

    /// Open a server-to-client subscription and return its live stream.
    pub async fn listen(
        &self,
        label: &str,
        filter: SubscriptionFilter,
    ) -> Result<Subscription, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.listen(filter).await
    }

    async fn validate_tool(
        &self,
        label: &str,
        tool: &str,
        arguments: &Value,
    ) -> Result<(), ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.validate_tool_arguments(tool, arguments)
    }

    /// The read-only metadata cache of one attached server.
    pub async fn cache(&self, label: &str) -> Option<McpMetadataCache> {
        let client = self.client(label).await?;
        client.metadata_cache().ok()
    }

    /// Every attached server's cache, for a catalogue or a report.
    pub async fn caches(&self) -> Vec<McpMetadataCache> {
        let clients = self
            .servers
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut caches = Vec::new();
        for client in clients {
            if let Ok(cache) = client.metadata_cache() {
                caches.push(cache);
            }
        }
        caches
    }

    /// Invoke one tool on one attached server, with argument validation.
    async fn call(
        &self,
        label: &str,
        tool: &str,
        arguments: Value,
        timeout_ms: u64,
    ) -> Result<Value, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        if timeout_ms == MCP_CALL_TIMEOUT_MS {
            client.call_tool(tool, arguments).await
        } else {
            client
                .call_tool_with_timeout(tool, arguments, timeout_ms)
                .await
        }
    }

    /// Read one resource only from the attached server's discovered catalogue.
    pub async fn read_resource(
        &self,
        label: &str,
        uri: &str,
    ) -> Result<McpResourceContent, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.read_resource(uri).await
    }

    /// Read one user-named resource URI, including a URI produced from a
    /// listed resource template.
    pub async fn read_resource_explicit(
        &self,
        label: &str,
        uri: &str,
    ) -> Result<McpResourceContent, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.read_resource_explicit(uri).await
    }

    /// Detach one server and stop its process.
    pub async fn detach(&self, label: &str) -> bool {
        let client = self.servers.lock().await.remove(label);
        match client {
            Some(client) => {
                if let Ok(client) = Arc::try_unwrap(client) {
                    client.close().await;
                }
                true
            }
            None => false,
        }
    }

    /// Stop one server after in-flight calls have drained, then cancel the
    /// transport at the grace deadline. The report never claims a call drained
    /// when the transport had to be canceled underneath it.
    pub async fn unload_draining(&self, label: &str, grace_ms: u64) -> McpUnloadReport {
        let Some(client) = self.servers.lock().await.remove(label) else {
            return McpUnloadReport {
                label: label.to_owned(),
                drained: true,
                inflight: 0,
            };
        };
        let deadline = Instant::now() + Duration::from_millis(grace_ms.min(CANCEL_GRACE_MS));
        while Arc::strong_count(&client) > 1 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let inflight = Arc::strong_count(&client).saturating_sub(1);
        match Arc::try_unwrap(client) {
            Ok(client) => {
                client.close().await;
                McpUnloadReport {
                    label: label.to_owned(),
                    drained: inflight == 0,
                    inflight,
                }
            }
            Err(client) => {
                drop(client);
                McpUnloadReport {
                    label: label.to_owned(),
                    drained: false,
                    inflight,
                }
            }
        }
    }

    /// Stop every attached server, giving each active call the M6 cancel grace.
    pub async fn drain_all(&self) -> Vec<McpUnloadReport> {
        let labels = self.attached_labels().await;
        let mut reports = Vec::with_capacity(labels.len());
        for label in labels {
            reports.push(self.unload_draining(&label, CANCEL_GRACE_MS).await);
        }
        reports
    }

    /// Stop every attached server, giving each active call the M6 cancel grace.
    pub async fn close_all(&self) {
        let _ = self.drain_all().await;
    }

    #[must_use]
    pub async fn attached_labels(&self) -> Vec<String> {
        self.servers.lock().await.keys().cloned().collect()
    }
}

/// Routes an MCP-originated tool call through the shared tool gate.
///
/// The host reaches MCP only by resolving a `CodingToolAction::ExternalTool`
/// into this dispatcher, which the `ToolService` calls *after* policy,
/// approval, intent and receipt handling. There is no second entry point: a
/// caller that has not passed the gate has no way to reach a server tool.
pub struct McpToolDispatcher {
    runtime: Arc<McpRuntime>,
}

impl std::fmt::Debug for McpToolDispatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpToolDispatcher")
            .finish_non_exhaustive()
    }
}

impl McpToolDispatcher {
    #[must_use]
    pub fn new(runtime: Arc<McpRuntime>) -> Self {
        Self { runtime }
    }
}

impl ExternalToolDispatcher for McpToolDispatcher {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), HarnessError>> + Send + 'a>>
    {
        Box::pin(async move {
            self.runtime
                .validate_tool(plugin_id, tool_name, arguments)
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a harness_tools::ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let payload = self
                .runtime
                .call(plugin_id, tool_name, arguments.clone(), timeout_ms)
                .await
                .map_err(|error| HarnessError::new(error.code(), error.to_string()))?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: plugin_id.to_owned(),
                tool_name: tool_name.to_owned(),
                payload,
                inflight: 0,
            })
        })
    }
}

/// Describe one advertised tool, refusing a schema the host cannot validate.
fn describe(tool: &Tool) -> Result<McpToolDescriptor, ExtensionError> {
    let input_schema =
        serde_json::to_value(&tool.input_schema).unwrap_or(serde_json::Value::Object(Map::new()));
    let description = tool
        .description
        .clone()
        .map_or_else(String::new, std::borrow::Cow::into_owned);
    let digest = harness_types::ContentHash::from_canonical_json(&serde_json::json!({
        "name": tool.name.as_ref(),
        "description": &description,
        "input_schema": &input_schema,
    }))
    .unwrap_or_else(|_| harness_types::ContentHash::from_bytes(tool.name.as_bytes()));
    let descriptor = McpToolDescriptor {
        name: tool.name.to_string(),
        description,
        input_schema,
        digest,
    };
    validate_tool_schema(&descriptor)?;
    Ok(descriptor)
}

fn describe_resource(
    resource: &rmcp::model::Resource,
) -> Result<McpResourceDescriptor, ExtensionError> {
    if resource.uri.trim().is_empty() {
        return Err(ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            "MCP server advertised a resource without a URI",
        ));
    }
    let description = resource.description.clone().unwrap_or_default();
    let digest = ContentHash::from_canonical_json(&serde_json::json!({
        "domain": "mcp-resource.v1",
        "uri": resource.uri,
        "name": resource.name,
        "description": description,
        "mime_type": resource.mime_type,
    }))
    .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;
    Ok(McpResourceDescriptor {
        uri: resource.uri.clone(),
        name: resource.name.clone(),
        description,
        mime_type: resource.mime_type.clone(),
        digest,
    })
}
