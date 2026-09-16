//! A minimal MCP server used as a P6 fixture.
//!
//! It speaks the real MCP protocol over stdio through the official SDK, so the
//! acceptance target exercises real client negotiation rather than a stub. It
//! advertises two tools and records nothing: it exists to prove the adapter
//! path, not to be a product server.

use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerConfig},
    tool, tool_handler, tool_router,
    transport::io::stdio,
};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
struct ObserveRequest {
    /// The subject to observe.
    subject: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NoteRequest {
    /// The note text to record in the fixture.
    text: String,
}

#[derive(Clone)]
struct FixtureServer {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl FixtureServer {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Record one source observation")]
    async fn observe(
        &self,
        Parameters(request): Parameters<ObserveRequest>,
    ) -> Result<String, McpError> {
        Ok(format!("observed {}", request.subject))
    }

    #[tool(description = "Write a note through the fixture extension path")]
    async fn write_note(
        &self,
        Parameters(request): Parameters<NoteRequest>,
    ) -> Result<String, McpError> {
        Ok(format!(
            "recorded note of {} characters",
            request.text.len()
        ))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("P6 fixture MCP server")
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let service = rmcp::serve_server(FixtureServer::new(), stdio()).await?;
    service.waiting().await?;
    Ok(())
}
