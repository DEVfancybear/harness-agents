//! The remote task service the M11-03 acceptance runs against.
//!
//! It is a real MCP server over stdio through the pinned SDK, and it declares
//! the SEP-2663 Tasks extension (`io.modelcontextprotocol/tasks`), so the client
//! under test negotiates the extension over the wire rather than being handed a
//! task-shaped object by a stub.
//!
//! Modes (`--mode`), chosen so a failure can be reproduced without shipping a
//! broken production path:
//!
//! - `normal` (default): every `long_job` call materialises a task, and the task
//!   completes after `--complete-after` `tasks/get` polls (default 2).
//! - `never_terminal`: the task stays `working` forever, so a poller with a
//!   deadline must stop by itself.
//! - `drop_after_accept`: the mutation is **applied and persisted**, then the
//!   process exits without answering. This is the case A35 is about: the request
//!   succeeded and the answer was lost.
//! - `refuse`: the call fails with a protocol error before anything is created,
//!   which is the *definite* failure - the remote answered and refused.
//! - `cancel_race`: `tasks/cancel` is acknowledged, but the work has already
//!   completed. Cancellation is cooperative, and an acknowledged cancel is not a
//!   rolled-back effect.
//!
//! `--state <path>` is the remote's own durable task table. Two processes
//! sharing one path are the same logical service across a restart, which is what
//! makes "poll a task this host submitted before it restarted" a real scenario
//! instead of a simulation.
//!
//! `--log <path>` appends one line per request, so the acceptance can count how
//! many times a mutation was sent: the proof that nothing was resubmitted.

use std::{collections::BTreeMap, io::Write, path::PathBuf, sync::Arc};

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, CancelTaskParams, GetTaskParams,
        GetTaskResult, Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
        ServerConfig, Task, TaskPayload, TaskStatus, Tool,
    },
    transport::io::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

/// One task as the remote service remembers it.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RemoteTask {
    polls: usize,
    cancelled: bool,
    completed: bool,
    #[serde(default)]
    complete_after: usize,
}

/// The remote's durable table.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct RemoteTable {
    next_id: u64,
    tasks: BTreeMap<String, RemoteTask>,
}

#[derive(Clone)]
struct TaskFixture {
    mode: Arc<String>,
    log: Option<Arc<PathBuf>>,
    state_path: Option<Arc<PathBuf>>,
    complete_after: usize,
    poll_interval_ms: Option<u64>,
    table: Arc<Mutex<RemoteTable>>,
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

/// The wire name of a status, which is what the log records.
fn status_name(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Working => "working",
        TaskStatus::InputRequired => "input_required",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        _ => "unknown",
    }
}

impl TaskFixture {
    fn tools() -> Vec<Tool> {
        vec![
            Tool::new(
                "long_job",
                "Start work the remote finishes later",
                object_schema(&json!({"subject": {"type": "string"}}), &json!(["subject"])),
            ),
            Tool::new(
                "quick_job",
                "Finish work inside the call",
                object_schema(&json!({"subject": {"type": "string"}}), &json!(["subject"])),
            ),
        ]
    }

    fn record(&self, line: &str) {
        let Some(path) = &self.log else {
            return;
        };
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_path())
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn save(&self, table: &RemoteTable) {
        let Some(path) = &self.state_path else {
            return;
        };
        if let Ok(body) = serde_json::to_string(table) {
            let _ = std::fs::write(path.as_path(), body);
        }
    }

    fn load(path: &std::path::Path) -> RemoteTable {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|body| serde_json::from_str::<RemoteTable>(&body).ok())
            .unwrap_or_default()
    }

    fn timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn task_for(&self, entry: &RemoteTask, id: &str) -> (Task, TaskPayload) {
        let stamp = Self::timestamp();
        let base = Task::new(id.to_owned(), TaskStatus::Working, stamp.clone(), stamp);
        let base = match self.poll_interval_ms {
            Some(interval) => base.with_poll_interval_ms(interval),
            None => base,
        };
        if self.mode.as_str() == "never_terminal" {
            return (base, TaskPayload::Working);
        }
        if entry.cancelled {
            return (
                base.with_status_message("cancelled"),
                TaskPayload::Cancelled,
            );
        }
        if entry.completed || entry.polls >= entry.complete_after {
            let result = json!({
                "job": id,
                "polls": entry.polls,
                "served_by": "m11_fixture_task_server",
            });
            let result = result.as_object().cloned().unwrap_or_default();
            return (
                base.with_status_message("completed"),
                TaskPayload::Completed { result },
            );
        }
        (base, TaskPayload::Working)
    }
}

impl ServerHandler for TaskFixture {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks()
                .build(),
        )
        .with_server_info(Implementation::new("m11_fixture_task_server", "1.0.0"))
        .with_instructions("M11-03 fixture remote task service")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(Self::tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let declared_tasks = context
            .client_capabilities()
            .is_some_and(|capabilities| capabilities.supports_tasks());
        self.record(&format!(
            "call_tool {} client_declared_tasks={declared_tasks}",
            request.name
        ));
        if request.name == "quick_job" {
            return Ok(CallToolResponse::Complete(CallToolResult::structured(
                json!({"tool": "quick_job", "done": true}),
            )));
        }
        if request.name != "long_job" {
            return Err(McpError::invalid_params("unknown fixture tool", None));
        }
        if self.mode.as_str() == "refuse" {
            self.record("long_job refused");
            return Err(McpError::invalid_params(
                "the fixture refuses to start long_job",
                None,
            ));
        }
        let mut table = self.table.lock().await;
        table.next_id += 1;
        let id = format!("remote-job-{}", table.next_id);
        table.tasks.insert(
            id.clone(),
            RemoteTask {
                polls: 0,
                cancelled: false,
                completed: false,
                complete_after: self.complete_after,
            },
        );
        self.save(&table);
        self.record(&format!("task_created={id}"));
        if self.mode.as_str() == "drop_after_accept" {
            // The mutation is applied and durable, and the answer never leaves.
            // This is the ambiguity A35 is about, produced at the boundary of a
            // real process rather than by a flag inside production code.
            self.record(&format!("answer_dropped={id}"));
            drop(table);
            std::process::exit(0);
        }
        let stamp = Self::timestamp();
        let task = Task::new(id, TaskStatus::Working, stamp.clone(), stamp)
            .with_status_message("accepted");
        let task = match self.poll_interval_ms {
            Some(interval) => task.with_poll_interval_ms(interval),
            None => task,
        };
        Ok(CallToolResponse::Task(rmcp::model::CreateTaskResult::new(
            task,
        )))
    }

    async fn get_task(
        &self,
        request: GetTaskParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, McpError> {
        let mut table = self.table.lock().await;
        let Some(entry) = table.tasks.get_mut(&request.task_id) else {
            return Err(McpError::invalid_params("unknown fixture task", None));
        };
        entry.polls += 1;
        let entry = entry.clone();
        self.save(&table);
        let (task, payload) = self.task_for(&entry, &request.task_id);
        self.record(&format!(
            "tasks_get={} state={} polls={}",
            request.task_id,
            status_name(payload.status()),
            entry.polls
        ));
        Ok(GetTaskResult::new(rmcp::model::DetailedTask::new(
            task, payload,
        )))
    }

    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _context: rmcp::service::RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let mut table = self.table.lock().await;
        let Some(entry) = table.tasks.get_mut(&request.task_id) else {
            return Err(McpError::invalid_params("unknown fixture task", None));
        };
        if self.mode.as_str() == "cancel_race" {
            // The work finished before the cancel landed. The acknowledgement is
            // still returned, because that is what cooperative cancellation
            // means: the request was accepted, not the rollback.
            entry.completed = true;
            self.save(&table);
            self.record(&format!("tasks_cancel_raced={}", request.task_id));
            return Ok(());
        }
        entry.cancelled = true;
        self.save(&table);
        self.record(&format!("tasks_cancel={}", request.task_id));
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut mode = "normal".to_owned();
    let mut log: Option<String> = None;
    let mut state: Option<String> = None;
    let mut complete_after = 2usize;
    let mut poll_interval_ms: Option<u64> = Some(50);
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--mode" => {
                if let Some(value) = arguments.next() {
                    mode = value;
                }
            }
            "--log" => log = arguments.next(),
            "--state" => state = arguments.next(),
            "--complete-after" => {
                if let Some(value) = arguments.next() {
                    complete_after = value.parse().unwrap_or(2);
                }
            }
            "--poll-interval-ms" => {
                poll_interval_ms = arguments.next().and_then(|value| value.parse().ok());
            }
            _ => {}
        }
    }
    let table = match &state {
        Some(path) => TaskFixture::load(std::path::Path::new(path)),
        None => RemoteTable::default(),
    };
    let service = rmcp::serve_server(
        TaskFixture {
            mode: Arc::new(mode),
            log: log.map(|value| Arc::new(PathBuf::from(value))),
            state_path: state.map(|value| Arc::new(PathBuf::from(value))),
            complete_after,
            poll_interval_ms,
            table: Arc::new(Mutex::new(table)),
        },
        stdio(),
    )
    .await?;
    service.waiting().await?;
    Ok(())
}
