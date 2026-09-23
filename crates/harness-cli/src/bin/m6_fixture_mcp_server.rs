//! A hostile or degraded MCP server used as the M6 acceptance fixture.
//!
//! It speaks the real MCP protocol over stdio through the official SDK, so the
//! acceptance target exercises real client negotiation rather than a stub, and
//! it switches behaviour with `M6_FIXTURE_MODE` so hostile conditions can be
//! reproduced without shipping hostile code into a production path.
//!
//! Modes:
//! - `normal` (default): two tools with valid object schemas, two resources.
//! - `bad_schema`: one tool advertises a schema that is not an object schema.
//! - `required_unknown`: one tool requires a property it does not declare.
//! - `schema_change`: the schema digest changes after the first listing.
//! - `flood`: `tools/list` pages forever, so the client's page bound must stop it.
//! - `read_failure`: `resources/read` returns a protocol error.
//! - `blob_resource`: the resource body is binary, which the host must refuse.
//! - `input_required`: a tool asks for another round; the host must not resend it implicitly.
//!
//! Every tool call is appended to the file named by `--log <path>`, when that
//! argument is given. That file is how the acceptance target proves a call the
//! gate refused never reached the server.
//!
//! The mode and the log path travel as command-line arguments rather than
//! environment variables on purpose: the adapter starts this process with the
//! minimal environment allowlist, so an environment-driven fixture would not be
//! reproducible through the real transport.

use std::{io::Write, sync::Arc};

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, InputRequiredResult,
        ListResourcesResult, ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
        ServerConfig, Tool,
    },
    transport::io::stdio,
};
use serde_json::{Map, Value, json};

#[derive(Clone)]
struct FixtureServer {
    mode: Arc<String>,
    log: Option<Arc<String>>,
}

fn object_schema(properties: &Value, required: &Value) -> Arc<Map<String, Value>> {
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    });
    Arc::new(schema.as_object().cloned().unwrap_or_default())
}

impl FixtureServer {
    fn tools(&self) -> Vec<Tool> {
        let observe = Tool::new(
            "observe",
            "Record one source observation",
            object_schema(&json!({"subject": {"type": "string"}}), &json!(["subject"])),
        );
        let note = Tool::new(
            "write_note",
            "Write a note through the fixture extension path",
            object_schema(&json!({"text": {"type": "string"}}), &json!(["text"])),
        );
        match self.mode.as_str() {
            "bad_schema" => {
                // Not an object schema: `type` says array while `properties`
                // describes an object, which the host must refuse.
                let mut broken = observe.clone();
                broken.input_schema =
                    object_schema(&json!({"subject": {"type": "string"}}), &json!(["subject"]));
                let mut raw = (*broken.input_schema).clone();
                raw.insert("type".to_owned(), Value::String("array".to_owned()));
                broken.input_schema = Arc::new(raw);
                vec![broken, note]
            }
            "required_unknown" => {
                let mut broken = observe.clone();
                let mut raw = (*broken.input_schema).clone();
                raw.insert(
                    "required".to_owned(),
                    json!(["subject", "not_declared_anywhere"]),
                );
                broken.input_schema = Arc::new(raw);
                vec![broken, note]
            }
            "schema_change" => {
                // The schema keeps a valid shape but changes content, so a
                // digest pinned before the change no longer matches.
                let mut changed = observe.clone();
                changed.input_schema = object_schema(
                    &json!({
                        "subject": {"type": "string"},
                        "revision": {"type": "string"},
                    }),
                    &json!(["subject"]),
                );
                vec![changed, note]
            }
            _ => vec![observe, note],
        }
    }

    fn resources() -> Vec<Resource> {
        vec![
            Resource::new("fixture://observations/one", "observation one")
                .with_description("first fixture observation")
                .with_mime_type("text/plain"),
            Resource::new("fixture://observations/two", "observation two")
                .with_description("second fixture observation")
                .with_mime_type("text/plain"),
        ]
    }

    fn record(&self, tool: &str) {
        let Some(path) = &self.log else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_str())
        {
            let _ = writeln!(file, "{tool}");
        }
    }
}

impl ServerHandler for FixtureServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions("M6 fixture MCP server")
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let cursor = request.and_then(|params| params.cursor);
        if self.mode.as_str() == "flood" {
            // Never stop offering another page. The client's page bound is the
            // only thing that can end this walk.
            let page = cursor
                .as_deref()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            let mut result = ListToolsResult::with_all_items(self.tools());
            result.next_cursor = Some((page + 1).to_string());
            return Ok(result);
        }
        if cursor.is_some() {
            // One page only: a second request means the client is walking past
            // the end, which must not happen.
            let mut result = ListToolsResult::with_all_items(Vec::new());
            result.next_cursor = None;
            return Ok(result);
        }
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(Self::resources()))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if self.mode.as_str() == "read_failure" {
            return Err(McpError::internal_error("fixture read failure", None));
        }
        let body = if self.mode.as_str() == "blob_resource" {
            ResourceContents::blob("AAECAwQ=", request.uri.clone())
        } else {
            ResourceContents::text(
                format!("fixture body for {}", request.uri),
                request.uri.clone(),
            )
            .with_mime_type("text/plain")
        };
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![body],
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.record(&request.name);
        if self.mode.as_str() == "input_required" {
            return Ok(CallToolResponse::InputRequired(
                InputRequiredResult::from_request_state("m6-fixture-state"),
            ));
        }
        let arguments = request.arguments.map_or(Value::Null, Value::Object);
        Ok(CallToolResponse::Complete(CallToolResult::structured(
            json!({
                "tool": request.name,
                "arguments": arguments,
                "served_by": "m6_fixture_mcp_server",
            }),
        )))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mode = "normal".to_owned();
    let mut log: Option<String> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--mode" => {
                if let Some(value) = arguments.next() {
                    mode = value;
                }
            }
            "--log" => log = arguments.next(),
            _ => {}
        }
    }
    let service = rmcp::serve_server(
        FixtureServer {
            mode: Arc::new(mode),
            log: log.map(Arc::new),
        },
        stdio(),
    )
    .await?;
    service.waiting().await?;
    Ok(())
}
