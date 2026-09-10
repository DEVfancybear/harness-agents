use std::path::PathBuf;

use harness_types::{
    ContentHash, SessionId, TaskId, ToolApprovalId, ToolExecutionId, ToolExecutionReceipt,
    ToolInvocationId, WorkspaceObservation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The independently versioned P3 tool contract.
pub const TOOL_CONTRACT_VERSION: u16 = 1;

/// A stable capability name used by policy, approvals, receipts, and UI views.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    ReadFile,
    ListFiles,
    SearchText,
    ApplyPatch,
    RunProcess,
    RunShell,
    GitStatus,
    GitDiff,
    TaskUpdate,
}

impl ToolKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::ListFiles => "list_files",
            Self::SearchText => "search_text",
            Self::ApplyPatch => "apply_patch",
            Self::RunProcess => "run_process",
            Self::RunShell => "run_shell",
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::TaskUpdate => "task_update",
        }
    }
}

/// Return the provider-function schemas accepted by the P3 parser. The
/// schemas are descriptive input contracts only: every call still crosses the
/// typed execution gate and its policy/approval checks.
#[must_use]
pub fn coding_tool_schemas() -> Vec<Value> {
    vec![
        function_schema(
            "read_file",
            "Read one bounded UTF-8 text file rooted in the registered workspace.",
            json!({"path": string_schema()}),
            &["path"],
        ),
        function_schema(
            "list_files",
            "List bounded non-sensitive files rooted in the registered workspace.",
            json!({"path": nullable_string_schema()}),
            &[],
        ),
        function_schema(
            "search_text",
            "Search bounded UTF-8 workspace text without following links.",
            json!({"query": string_schema(), "path": nullable_string_schema()}),
            &["query"],
        ),
        function_schema(
            "apply_patch",
            "Replace one UTF-8 text file only when its exact expected hash still matches.",
            json!({
                "path": string_schema(),
                "expected_hash": string_schema(),
                "replacement": string_schema()
            }),
            &["path", "expected_hash", "replacement"],
        ),
        function_schema(
            "run_process",
            "Run an explicitly structured executable and argv in the workspace.",
            json!({
                "executable": string_schema(),
                "args": {"type": "array", "items": string_schema()},
                "timeout_ms": {"type": "integer", "minimum": 1},
                "isolation": isolation_schema()
            }),
            &["executable", "args", "timeout_ms"],
        ),
        function_schema(
            "run_shell",
            "Run an explicitly requested shell command; never use this for structured argv.",
            json!({
                "command": string_schema(),
                "timeout_ms": {"type": "integer", "minimum": 1},
                "isolation": isolation_schema()
            }),
            &["command", "timeout_ms"],
        ),
        function_schema(
            "git_status",
            "Inspect Git status without changing the workspace.",
            json!({}),
            &[],
        ),
        function_schema(
            "git_diff",
            "Inspect Git diff without changing the workspace.",
            json!({"path": nullable_string_schema()}),
            &[],
        ),
        function_schema(
            "task_update",
            "Persist a bounded next-action note without fabricating a process receipt.",
            json!({"note": string_schema()}),
            &["note"],
        ),
    ]
}

#[allow(clippy::needless_pass_by_value)]
fn function_schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": description,
            "parameters": {
                "type": "object",
                "additionalProperties": false,
                "properties": properties,
                "required": required
            }
        }
    })
}

fn string_schema() -> Value {
    json!({"type": "string"})
}

fn nullable_string_schema() -> Value {
    json!({"type": ["string", "null"]})
}

fn isolation_schema() -> Value {
    json!({"type": "string", "enum": ["best_effort", "strict"]})
}

/// Isolation claims are deliberately small. `Strict` must be denied until a
/// verified sandbox backend exists instead of being mapped to best effort.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationMode {
    BestEffort,
    Strict,
}

/// Canonical P3 coding actions. Shell text has a separate variant so it can
/// never be mistaken for structured executable/argv execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodingToolAction {
    ReadFile {
        path: String,
    },
    ListFiles {
        path: Option<String>,
    },
    SearchText {
        query: String,
        path: Option<String>,
    },
    ApplyPatch {
        path: String,
        expected_hash: ContentHash,
        replacement: String,
    },
    RunProcess {
        executable: String,
        args: Vec<String>,
        timeout_ms: u64,
        isolation: IsolationMode,
    },
    RunShell {
        command: String,
        timeout_ms: u64,
        isolation: IsolationMode,
    },
    GitStatus,
    GitDiff {
        path: Option<String>,
    },
    TaskUpdate {
        note: String,
    },
}

impl CodingToolAction {
    #[must_use]
    pub const fn kind(&self) -> ToolKind {
        match self {
            Self::ReadFile { .. } => ToolKind::ReadFile,
            Self::ListFiles { .. } => ToolKind::ListFiles,
            Self::SearchText { .. } => ToolKind::SearchText,
            Self::ApplyPatch { .. } => ToolKind::ApplyPatch,
            Self::RunProcess { .. } => ToolKind::RunProcess,
            Self::RunShell { .. } => ToolKind::RunShell,
            Self::GitStatus => ToolKind::GitStatus,
            Self::GitDiff { .. } => ToolKind::GitDiff,
            Self::TaskUpdate { .. } => ToolKind::TaskUpdate,
        }
    }

    #[must_use]
    pub fn path_hint(&self) -> Option<&str> {
        match self {
            Self::ReadFile { path } | Self::ApplyPatch { path, .. } => Some(path),
            Self::ListFiles { path } | Self::SearchText { path, .. } | Self::GitDiff { path } => {
                path.as_deref()
            }
            Self::RunProcess { .. }
            | Self::RunShell { .. }
            | Self::GitStatus
            | Self::TaskUpdate { .. } => None,
        }
    }

    #[must_use]
    pub const fn has_external_side_effect(&self) -> bool {
        matches!(
            self,
            Self::ApplyPatch { .. } | Self::RunProcess { .. } | Self::RunShell { .. }
        )
    }

    pub fn canonical_value(&self) -> Result<Value, harness_types::HarnessError> {
        serde_json::to_value(self).map_err(|_| {
            harness_types::HarnessError::new(
                harness_types::ErrorCode::InvalidPayload,
                "tool action cannot be serialized",
            )
        })
    }

    pub fn canonical_hash(&self) -> Result<ContentHash, harness_types::HarnessError> {
        let value = self.canonical_value()?;
        ContentHash::from_canonical_json(&value)
    }

    /// Convert only the supported P3 provider-function schema into a typed
    /// action. Incomplete JSON never reaches the execution gate.
    #[allow(clippy::too_many_lines)]
    pub fn from_provider_call(
        name: &str,
        arguments: &str,
    ) -> Result<Self, harness_types::HarnessError> {
        let value: Value = serde_json::from_str(arguments).map_err(|_| {
            harness_types::HarnessError::new(
                harness_types::ErrorCode::ProviderProtocol,
                "provider tool arguments are incomplete or invalid JSON",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            harness_types::HarnessError::new(
                harness_types::ErrorCode::InvalidPayload,
                "provider tool arguments must be a JSON object",
            )
        })?;
        match name {
            "read_file" => Ok(Self::ReadFile {
                path: required_string(object, "path")?,
            }),
            "list_files" => Ok(Self::ListFiles {
                path: object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }),
            "search_text" => Ok(Self::SearchText {
                query: required_string(object, "query")?,
                path: object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }),
            "apply_patch" => Ok(Self::ApplyPatch {
                path: required_string(object, "path")?,
                expected_hash: ContentHash::parse(required_string(object, "expected_hash")?)?,
                replacement: required_string(object, "replacement")?,
            }),
            "run_process" => {
                let args = object
                    .get("args")
                    .and_then(Value::as_array)
                    .ok_or_else(|| {
                        harness_types::HarnessError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "provider process args must be an array",
                        )
                    })?
                    .iter()
                    .map(|value| {
                        value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                            harness_types::HarnessError::new(
                                harness_types::ErrorCode::InvalidPayload,
                                "provider process args must contain strings",
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let timeout_ms = object
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        harness_types::HarnessError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "provider process timeout_ms is missing or invalid",
                        )
                    })?;
                Ok(Self::RunProcess {
                    executable: required_string(object, "executable")?,
                    args,
                    timeout_ms,
                    isolation: parse_isolation(object.get("isolation"))?,
                })
            }
            "run_shell" => Ok(Self::RunShell {
                command: required_string(object, "command")?,
                timeout_ms: object
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| {
                        harness_types::HarnessError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "provider shell timeout_ms is missing or invalid",
                        )
                    })?,
                isolation: parse_isolation(object.get("isolation"))?,
            }),
            "git_status" => Ok(Self::GitStatus),
            "git_diff" => Ok(Self::GitDiff {
                path: object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }),
            "task_update" => Ok(Self::TaskUpdate {
                note: required_string(object, "note")?,
            }),
            _ => Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::PolicyDenied,
                format!("provider requested unsupported P3 tool {name}"),
            )),
        }
    }
}

fn required_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, harness_types::HarnessError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            harness_types::HarnessError::new(
                harness_types::ErrorCode::InvalidPayload,
                format!("provider tool field {key} is missing or invalid"),
            )
        })
}

fn parse_isolation(value: Option<&Value>) -> Result<IsolationMode, harness_types::HarnessError> {
    match value.and_then(Value::as_str).unwrap_or("best_effort") {
        "best_effort" => Ok(IsolationMode::BestEffort),
        "strict" => Ok(IsolationMode::Strict),
        _ => Err(harness_types::HarnessError::new(
            harness_types::ErrorCode::InvalidPayload,
            "provider isolation is invalid",
        )),
    }
}

/// Proposed work supplied by a model or a CLI caller. It is not an execution
/// authority until transformed into a [`PreparedToolRequest`] and approved.
#[derive(Clone, Debug)]
pub struct ToolRequest {
    pub session_id: SessionId,
    pub task_id: TaskId,
    pub actor_id: String,
    pub invocation_id: ToolInvocationId,
    pub workspace_root: PathBuf,
    pub action: CodingToolAction,
}

impl ToolRequest {
    #[must_use]
    pub fn new(
        session_id: SessionId,
        task_id: TaskId,
        actor_id: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        action: CodingToolAction,
    ) -> Self {
        Self {
            session_id,
            task_id,
            actor_id: actor_id.into(),
            invocation_id: ToolInvocationId::generate(),
            workspace_root: workspace_root.into(),
            action,
        }
    }
}

/// A capability-aware display of what P3 can genuinely enforce on this host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolCapabilities {
    pub schema_version: u16,
    pub process_tree_cleanup: String,
    pub strict_isolation: bool,
    pub shell_requires_explicit_action: bool,
    pub filesystem_network_sandbox: bool,
}

/// A finalized, canonical action whose approval must bind exactly to the
/// fields below. It remains a proposal until `execute` records its intent.
#[derive(Clone, Debug)]
pub struct PreparedToolRequest {
    pub(crate) request: ToolRequest,
    pub(crate) final_action: CodingToolAction,
    pub(crate) action_hash: ContentHash,
    pub(crate) workspace_root: PathBuf,
    pub(crate) workspace_root_text: String,
    pub(crate) workspace_fingerprint: ContentHash,
    pub(crate) workspace_identity_hash: ContentHash,
    pub(crate) project_id: harness_types::ProjectId,
    pub(crate) policy_revision: u64,
    pub(crate) policy_denial: Option<String>,
}

impl PreparedToolRequest {
    #[must_use]
    pub fn action(&self) -> &CodingToolAction {
        &self.final_action
    }

    #[must_use]
    pub fn action_hash(&self) -> &ContentHash {
        &self.action_hash
    }

    #[must_use]
    pub fn workspace_fingerprint(&self) -> &ContentHash {
        &self.workspace_fingerprint
    }

    #[must_use]
    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }
}

/// A persisted one-use approval bound to the exact final canonical request.
#[derive(Clone, Debug)]
pub struct ApprovalGrant {
    pub(crate) approval_id: ToolApprovalId,
    pub(crate) actor_id: String,
    pub(crate) action_hash: ContentHash,
    pub(crate) workspace_root: String,
    pub(crate) workspace_fingerprint: ContentHash,
    pub(crate) policy_revision: u64,
    pub(crate) tool_revision: u64,
    pub(crate) expires_at_unix_ms: Option<u64>,
}

impl ApprovalGrant {
    #[must_use]
    pub fn approval_id(&self) -> &ToolApprovalId {
        &self.approval_id
    }
}

/// Bounded, redacted tool output presented to a CLI/model. The durable
/// receipt, not this value, is the authoritative outcome.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolOutput {
    ReadFile {
        path: String,
        content: String,
        truncated: bool,
    },
    ListFiles {
        paths: Vec<String>,
        truncated: bool,
    },
    SearchText {
        matches: Vec<SearchMatch>,
        truncated: bool,
    },
    ApplyPatch {
        path: String,
        before_hash: ContentHash,
        after_hash: ContentHash,
    },
    Process {
        executable: String,
        exit_code: Option<i32>,
        timed_out: bool,
        canceled: bool,
        tree_cleanup_confirmed: bool,
        stdout: String,
        stderr: String,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    Git {
        operation: String,
        output: String,
        truncated: bool,
    },
    TaskUpdate {
        note: String,
    },
    Denied {
        code: String,
        reason: String,
    },
    OutcomeUnknown {
        reason: String,
    },
}

/// A bounded match returned by `search_text`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SearchMatch {
    pub path: String,
    pub line: u64,
    pub column: u64,
    pub preview: String,
}

/// The immutable receipt plus a non-authoritative presentation projection.
#[derive(Clone, Debug, Serialize)]
pub struct ToolExecutionView {
    pub schema_version: u16,
    pub execution_id: Option<ToolExecutionId>,
    pub receipt: Option<ToolExecutionReceipt>,
    pub output: ToolOutput,
    pub observer_failure: Option<String>,
}

/// Build a P2-compatible observation from a real rooted workspace. This is a
/// read-only configuration helper; P3 execution re-observes it inside the gate.
pub(crate) fn observation(
    project_id: harness_types::ProjectId,
    worktree_id: String,
    base_commit: String,
    fingerprint: ContentHash,
) -> WorkspaceObservation {
    WorkspaceObservation {
        project_id,
        worktree_id,
        base_commit,
        observed_fingerprint: fingerprint,
    }
}
