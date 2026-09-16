//! MCP client adapter.
//!
//! MCP negotiation belongs to the official `rmcp` SDK contract and is never
//! assumed compatible with this repository's custom plugin protocol. The
//! adapter discovers tools from a real MCP server process, registers them in a
//! scoped registry, and hands execution back to the P3 gate: an MCP-originated
//! call is authorized exactly like any other tool call.
//!
//! Only local stdio servers are supported. Remote endpoints would need an
//! explicit trust and network policy and are out of scope for this phase.

use std::{collections::BTreeMap, path::PathBuf, process::Stdio};

use harness_kernel::{RegistrationToken, ScopedRegistry};
use harness_types::{ErrorCode, PluginInstanceId, ScopeId};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use rmcp::{
    RoleClient, ServiceExt,
    model::{CallToolRequestParams, Tool},
    transport::TokioChildProcess,
};

use crate::contracts::ExtensionError;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;

/// One tool discovered from an MCP server.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolDescriptor {
    pub name: String,
    pub description: String,
    /// The JSON schema the server advertised, recorded so a change is visible.
    pub input_schema: serde_json::Value,
    pub digest: harness_types::ContentHash,
}

/// A live MCP client bound to one server process.
pub struct McpClient {
    scope_id: ScopeId,
    generation: u64,
    server_label: String,
    executable: PathBuf,
    running: rmcp::service::RunningService<RoleClient, ()>,
    tools: BTreeMap<String, McpToolDescriptor>,
    tokens: Vec<RegistrationToken>,
}

impl std::fmt::Debug for McpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpClient")
            .field("server", &self.server_label)
            .field("tools", &self.tools.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl McpClient {
    /// Start a local MCP server over stdio, complete the SDK handshake, and
    /// discover its tools.
    ///
    /// Discovery is page-bounded: the adapter never loops forever asking for
    /// more pages from an untrusted server.
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
        let running = ().serve(transport).await.map_err(|error| {
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
            tokens: Vec::new(),
        };
        client.discover_tools().await?;
        Ok(client)
    }

    /// Discover tools with a bounded page walk.
    pub async fn discover_tools(&mut self) -> Result<usize, ExtensionError> {
        const MAX_PAGES: usize = 8;
        let mut discovered: BTreeMap<String, McpToolDescriptor> = BTreeMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor.clone().map(|cursor| {
                rmcp::model::PaginatedRequestParams::default().with_cursor(Some(cursor))
            });
            let page = self.running.list_tools(params).await.map_err(|error| {
                ExtensionError::new(
                    ErrorCode::ExtensionProtocolError,
                    format!("MCP list_tools failed: {error}"),
                )
            })?;
            for tool in page.tools {
                let descriptor = describe(&tool);
                discovered.insert(descriptor.name.clone(), descriptor);
            }
            match page.next_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => break,
            }
        }
        self.tools = discovered;
        Ok(self.tools.len())
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

    /// Call one discovered tool. An unknown tool is refused locally rather than
    /// forwarded to the server.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, ExtensionError> {
        if !self.tools.contains_key(name) {
            return Err(ExtensionError::new(
                ErrorCode::PolicyDenied,
                format!("MCP server did not advertise a tool named {name}"),
            ));
        }
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        let result = self
            .running
            .call_tool(CallToolRequestParams::new(name.to_owned()).with_arguments(arguments))
            .await
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
    pub fn schema_drift(
        &self,
        pinned: &BTreeMap<String, harness_types::ContentHash>,
    ) -> Vec<String> {
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

    #[must_use]
    pub fn tools(&self) -> &BTreeMap<String, McpToolDescriptor> {
        &self.tools
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

fn describe(tool: &Tool) -> McpToolDescriptor {
    let input_schema = serde_json::to_value(&tool.input_schema)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
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
    McpToolDescriptor {
        name: tool.name.to_string(),
        description,
        input_schema,
        digest,
    }
}
