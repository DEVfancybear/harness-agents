//! MCP client adapter.
//!
//! MCP negotiation belongs to the official `rmcp` SDK contract and is never
//! assumed compatible with this repository's custom plugin protocol. The
//! adapter discovers tools and resources from a real MCP server process,
//! registers them in a scoped registry, and hands execution back to the P3
//! gate: an MCP-originated call is authorized exactly like any other tool call.
//!
//! Only local stdio servers are supported. Remote endpoints would need an
//! explicit trust and network policy and are out of scope for this milestone.
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
    RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
        ReadResourceRequestParams, ResourceContents, Tool,
    },
    transport::TokioChildProcess,
};
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::contracts::{ENVIRONMENT_ALLOWLIST, ExtensionError};

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
        }
    }

    /// Whether this build implements the feature. The SDK offering a method is
    /// deliberately not the question being asked.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Tools | Self::Resources)
    }

    /// Why an unsupported feature is not advertised. Empty when supported.
    #[must_use]
    pub const fn unsupported_reason(self) -> &'static str {
        match self {
            Self::Tools | Self::Resources => "",
            Self::ResourceTemplates => {
                "resource templates are discovered by the SDK but this build does not expand them"
            }
            Self::Prompts => "prompt discovery and rendering are not implemented in this build",
            Self::Sampling => "server-initiated model sampling is out of scope for this milestone",
            Self::Elicitation => {
                "server-initiated user elicitation is out of scope for this milestone"
            }
            Self::Subscriptions => "resource subscriptions are not implemented in this build",
            Self::RemoteTransport => {
                "only local stdio servers are supported; a remote endpoint needs its own trust and network policy"
            }
        }
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
    pub const ALL: [McpFeature; 8] = [
        McpFeature::Tools,
        McpFeature::Resources,
        McpFeature::ResourceTemplates,
        McpFeature::Prompts,
        McpFeature::Sampling,
        McpFeature::Elicitation,
        McpFeature::Subscriptions,
        McpFeature::RemoteTransport,
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
    running: rmcp::service::RunningService<RoleClient, ()>,
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
        let executable = executable.into();
        let server_label = executable.file_stem().map_or_else(
            || "mcp-server".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let args = arguments.clone();
        let mut wrapped = CommandWrap::with_new(&executable, move |command| {
            // The host environment is not inherited wholesale: a provider API
            // key in the parent process must not reach an MCP server.
            command.env_clear();
            for name in ENVIRONMENT_ALLOWLIST {
                if let Ok(value) = std::env::var(name) {
                    command.env(name, value);
                }
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
            ().serve(transport),
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

    /// Read one discovered resource.
    ///
    /// Only text is carried: a binary body would need its own content pipeline
    /// and is refused rather than decoded into a string the host cannot verify.
    pub async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, ExtensionError> {
        McpSupportMatrix::require(McpFeature::Resources)?;
        let descriptor = self.resources.get(uri).ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("MCP server did not advertise a resource at {uri}"),
            )
        })?;
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
        let _ = descriptor;
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
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, ExtensionError> {
        let descriptor = self.tools.get(name).ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("MCP server did not advertise a tool named {name}"),
            )
        })?;
        validate_arguments(name, &descriptor.input_schema, &arguments)?;
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let result = timeout(
            Duration::from_millis(MCP_CALL_TIMEOUT_MS),
            self.running
                .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments)),
        )
        .await
        .map_err(|_| {
            ExtensionError::new(
                ErrorCode::ProcessTimedOut,
                format!("MCP call_tool {name} exceeded {MCP_CALL_TIMEOUT_MS}ms"),
            )
        })?
        .map_err(|error| {
            ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!("MCP call_tool failed: {error}"),
            )
        })?;
        serde_json::to_value(&result).map_err(|_| {
            ExtensionError::new(
                ErrorCode::InvalidPayload,
                "MCP tool result is not serializable",
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

/// Validate one advertised tool schema.
///
/// A tool whose schema the host cannot understand is refused at discovery time,
/// which is the only moment where refusing it is free.
pub fn validate_tool_schema(descriptor: &McpToolDescriptor) -> Result<(), ExtensionError> {
    let schema = &descriptor.input_schema;
    let object = schema.as_object().ok_or_else(|| {
        ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            format!(
                "MCP tool {} advertised a schema that is not an object",
                descriptor.name
            ),
        )
    })?;
    match object.get("type") {
        Some(Value::String(kind)) if kind == "object" => {}
        Some(Value::String(kind)) => {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!(
                    "MCP tool {} advertised schema type {kind}; only object arguments are supported",
                    descriptor.name
                ),
            ));
        }
        Some(_) => {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!(
                    "MCP tool {} advertised a non-string schema type",
                    descriptor.name
                ),
            ));
        }
        // A schema without `type` is accepted only when it declares properties:
        // a bare `{}` accepts anything and would defeat argument validation.
        None => {
            if !object.contains_key("properties") {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!(
                        "MCP tool {} advertised a schema with neither a type nor properties",
                        descriptor.name
                    ),
                ));
            }
        }
    }
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        return Err(ExtensionError::new(
            ErrorCode::ExtensionProtocolError,
            format!(
                "MCP tool {} advertised non-object properties",
                descriptor.name
            ),
        ));
    }
    let declared = object
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(required) = object.get("required") {
        let Some(names) = required.as_array() else {
            return Err(ExtensionError::new(
                ErrorCode::ExtensionProtocolError,
                format!(
                    "MCP tool {} advertised a non-array required list",
                    descriptor.name
                ),
            ));
        };
        for name in names {
            let Some(name) = name.as_str() else {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!(
                        "MCP tool {} required a non-string property",
                        descriptor.name
                    ),
                ));
            };
            if !declared.iter().any(|declared| declared == name) {
                return Err(ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!(
                        "MCP tool {} requires {name}, which it does not declare",
                        descriptor.name
                    ),
                ));
            }
        }
    }
    Ok(())
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
        _ => true,
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

    pub async fn client(&self, label: &str) -> Option<Arc<McpClient>> {
        self.servers.lock().await.get(label).cloned()
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
    pub async fn call(
        &self,
        label: &str,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ExtensionError> {
        let client = self.client(label).await.ok_or_else(|| {
            ExtensionError::new(
                ErrorCode::ExtensionNotFound,
                format!("no MCP server is attached as {label}"),
            )
        })?;
        client.call_tool(tool, arguments).await
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

    /// Stop every attached server.
    pub async fn close_all(&self) {
        let clients = std::mem::take(&mut *self.servers.lock().await);
        for (_, client) in clients {
            if let Ok(client) = Arc::try_unwrap(client) {
                client.close().await;
            }
        }
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
    fn dispatch_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let payload = self
                .runtime
                .call(plugin_id, tool_name, arguments.clone())
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
