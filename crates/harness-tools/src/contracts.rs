use std::path::PathBuf;

use harness_types::{
    ContentHash, SessionId, TaskId, ToolApprovalId, ToolExecutionId, ToolExecutionReceipt,
    ToolInvocationId, WorkspaceObservation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The independently versioned P3 tool contract.
pub const TOOL_CONTRACT_VERSION: u16 = 1;

/// The only accepted shape of an environment reference a process action may
/// name. A model never supplies an environment *value*: it names a reference,
/// and the host resolves it immediately before spawn from a source the operator
/// granted. A literal value would let the model inject anything into the child,
/// so it is refused instead of being passed through.
pub const ENV_REFERENCE_PREFIX: &str = "secret://";

/// How many environment references one process action may carry.
pub const MAX_ENV_BINDINGS: usize = 8;

/// Longest accepted environment variable name.
pub const MAX_ENV_NAME_LEN: usize = 64;

/// Longest accepted secret reference, including its scheme.
pub const MAX_ENV_REFERENCE_LEN: usize = 200;

/// One environment variable a process action asks the host to set, named by a
/// reference rather than by a value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvBinding {
    /// The name the child sees.
    pub name: String,
    /// The reference the host resolves just before spawn (`secret://NAME`).
    pub reference: String,
}

impl EnvBinding {
    /// Validate one binding without resolving anything: shape only.
    pub fn validate(&self) -> Result<(), harness_types::HarnessError> {
        if self.name.len() > MAX_ENV_NAME_LEN || !is_environment_name(&self.name) {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "environment binding name must be a portable variable name",
            ));
        }
        if self.reference.len() > MAX_ENV_REFERENCE_LEN {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "environment binding reference is too long",
            ));
        }
        let Some(target) = self.reference.strip_prefix(ENV_REFERENCE_PREFIX) else {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "environment values are never taken from the model: use a secret:// reference",
            ));
        };
        if target.is_empty() || !is_environment_name(target) {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "secret reference must name a portable variable name",
            ));
        }
        Ok(())
    }
}

/// Which captured stream a paged read asks for.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStream {
    #[default]
    Stdout,
    Stderr,
}

impl CaptureStream {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

const fn default_page_length() -> u32 {
    PROCESS_OUTPUT_PAGE_DEFAULT_BYTES
}

fn is_environment_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Validate every environment binding of one action and return them in a
/// stable order, so the action hash does not depend on the model's key order.
pub fn normalize_env_bindings(
    bindings: &[EnvBinding],
) -> Result<Vec<EnvBinding>, harness_types::HarnessError> {
    if bindings.len() > MAX_ENV_BINDINGS {
        return Err(harness_types::HarnessError::new(
            harness_types::ErrorCode::EnvironmentDenied,
            format!("a process action may carry at most {MAX_ENV_BINDINGS} environment bindings"),
        ));
    }
    let mut normalized = bindings.to_vec();
    normalized.sort_by(|left, right| left.name.cmp(&right.name));
    for (index, binding) in normalized.iter().enumerate() {
        binding.validate()?;
        if index > 0 && normalized[index - 1].name == binding.name {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "environment binding names must be unique",
            ));
        }
    }
    Ok(normalized)
}

/// Default number of commits `git_log` returns when the model names no limit.
pub const GIT_LOG_DEFAULT_LIMIT: u32 = 20;
/// Largest page `read_process_output` returns in one call.
pub const PROCESS_OUTPUT_PAGE_MAX_BYTES: u32 = 64 * 1024;

/// Page size used when a caller names no length.
pub const PROCESS_OUTPUT_PAGE_DEFAULT_BYTES: u32 = 16 * 1024;
/// Hard ceiling for `git_log`: a model may ask for fewer, never for unbounded
/// history. The provider schema and the typed action both carry this bound.
pub const GIT_LOG_MAX_LIMIT: u32 = 100;

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
    ReadProcessOutput,
    GitStatus,
    GitDiff,
    GitLog,
    TaskUpdate,
    /// An external extension tool, reachable only through the same policy gate.
    ExternalTool,
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
            Self::ReadProcessOutput => "read_process_output",
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::GitLog => "git_log",
            Self::TaskUpdate => "task_update",
            Self::ExternalTool => "external_tool",
        }
    }

    /// Whether this action only reads.
    ///
    /// Reading is the one capability that cannot damage the workspace, so the host
    /// is allowed to stop asking for it. The list is deliberately an allowlist and
    /// nothing else: a kind is read-only because it was classified here, never
    /// because its name suggests it.
    ///
    /// This predicate is not the whole guard. A path is checked against the
    /// workspace root - traversal, symlinks and credential-like names - by
    /// `ToolExecutionService::prepare` *before* any proposal exists, and a paged
    /// artifact read is checked against the task that owns the artifact, so a read
    /// that is read-only by kind can still be refused outright. Auto-approving
    /// skips the question, never the check.
    #[must_use]
    pub const fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::ReadFile
                | Self::ListFiles
                | Self::SearchText
                | Self::ReadProcessOutput
                | Self::GitStatus
                | Self::GitDiff
                | Self::GitLog
        )
    }
}

/// The built-in provider-function names the P3 parser accepts.
///
/// A host that also advertises external tools needs this list to keep the two
/// namespaces apart: an extension may add a tool, never shadow a built-in one.
#[must_use]
pub const fn coding_tool_names() -> &'static [&'static str] {
    &[
        "read_file",
        "list_files",
        "search_text",
        "apply_patch",
        "run_process",
        "run_shell",
        "read_process_output",
        "git_status",
        "git_diff",
        "git_log",
        "task_update",
    ]
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
                "isolation": isolation_schema(),
                "env": environment_schema()
            }),
            &["executable", "args", "timeout_ms"],
        ),
        function_schema(
            "run_shell",
            "Run an explicitly requested shell command; never use this for structured argv.",
            json!({
                "command": string_schema(),
                "timeout_ms": {"type": "integer", "minimum": 1},
                "isolation": isolation_schema(),
                "env": environment_schema()
            }),
            &["command", "timeout_ms"],
        ),
        function_schema(
            "read_process_output",
            "Read one bounded page of a captured process output artifact owned by this task.",
            json!({
                "artifact_id": string_schema(),
                "stream": {"type": "string", "enum": ["stdout", "stderr"]},
                "offset": {"type": "integer", "minimum": 0},
                "length": {"type": "integer", "minimum": 1, "maximum": PROCESS_OUTPUT_PAGE_MAX_BYTES}
            }),
            &["artifact_id"],
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
            "git_log",
            "Inspect recent Git history as bounded tab-separated fields without changing the workspace.",
            json!({
                "path": nullable_string_schema(),
                "limit": {"type": "integer", "minimum": 1, "maximum": GIT_LOG_MAX_LIMIT}
            }),
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

/// The child environment is never inherited. A call may name `secret://`
/// references the host is willing to resolve; anything else is refused, so a
/// model cannot hand a literal value to a process it starts.
fn environment_schema() -> Value {
    json!({
        "type": "object",
        "maxProperties": MAX_ENV_BINDINGS,
        "additionalProperties": {"type": "string"},
        "description": "Environment variables resolved by the host from secret:// references; literal values are refused."
    })
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
        /// Environment variables the host resolves just before spawn. Empty for
        /// a process that needs nothing beyond the host allowlist; the field is
        /// skipped when empty so pre-M4 action hashes stay byte-identical.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env: Vec<EnvBinding>,
    },
    RunShell {
        command: String,
        timeout_ms: u64,
        isolation: IsolationMode,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        env: Vec<EnvBinding>,
    },
    /// Read a page of a captured process output. The artifact must belong to the
    /// calling task: an artifact id is not a capability by itself.
    ReadProcessOutput {
        artifact_id: String,
        #[serde(default)]
        stream: CaptureStream,
        #[serde(default)]
        offset: u64,
        #[serde(default = "default_page_length")]
        length: u32,
    },
    GitStatus,
    GitDiff {
        path: Option<String>,
    },
    GitLog {
        path: Option<String>,
        limit: Option<u32>,
    },
    TaskUpdate {
        note: String,
    },
    /// A tool provided by a trusted external extension. It crosses the same
    /// gate as every built-in action, and carries the parent invocation so a
    /// nested call stays correlated and non-escalating.
    ExternalTool {
        plugin_id: String,
        tool_name: String,
        arguments: Value,
        /// Parent invocation when this call was produced by another tool.
        parent_invocation_id: Option<String>,
        timeout_ms: u64,
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
            Self::ReadProcessOutput { .. } => ToolKind::ReadProcessOutput,
            Self::GitStatus => ToolKind::GitStatus,
            Self::GitDiff { .. } => ToolKind::GitDiff,
            Self::GitLog { .. } => ToolKind::GitLog,
            Self::TaskUpdate { .. } => ToolKind::TaskUpdate,
            Self::ExternalTool { .. } => ToolKind::ExternalTool,
        }
    }

    #[must_use]
    pub fn path_hint(&self) -> Option<&str> {
        match self {
            Self::ReadFile { path } | Self::ApplyPatch { path, .. } => Some(path),
            Self::ListFiles { path } | Self::SearchText { path, .. } | Self::GitDiff { path } => {
                path.as_deref()
            }
            Self::GitLog { path, .. } => path.as_deref(),
            Self::RunProcess { .. }
            | Self::RunShell { .. }
            | Self::ReadProcessOutput { .. }
            | Self::GitStatus
            | Self::TaskUpdate { .. }
            | Self::ExternalTool { .. } => None,
        }
    }

    #[must_use]
    pub const fn has_external_side_effect(&self) -> bool {
        matches!(
            self,
            Self::ApplyPatch { .. }
                | Self::RunProcess { .. }
                | Self::RunShell { .. }
                | Self::ExternalTool { .. }
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
        let allowed: &[&str] = match name {
            "read_file" | "list_files" | "git_diff" => &["path"],
            "search_text" => &["query", "path"],
            "apply_patch" => &["path", "expected_hash", "replacement"],
            "run_process" => &["executable", "args", "timeout_ms", "isolation", "env"],
            "run_shell" => &["command", "timeout_ms", "isolation", "env"],
            "git_status" => &[],
            "read_process_output" => &["artifact_id", "stream", "offset", "length"],
            "git_log" => &["path", "limit"],
            "task_update" => &["note"],
            _ => {
                return Err(harness_types::HarnessError::new(
                    harness_types::ErrorCode::PolicyDenied,
                    "provider requested an unsupported P3 tool",
                ));
            }
        };
        if object.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::InvalidPayload,
                "provider tool arguments contain an unknown field",
            ));
        }
        if matches!(name, "list_files" | "search_text" | "git_diff" | "git_log")
            && object
                .get("path")
                .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(harness_types::HarnessError::new(
                harness_types::ErrorCode::InvalidPayload,
                "provider tool path must be a string or null",
            ));
        }
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
                    env: parse_env_bindings(object.get("env"))?,
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
                env: parse_env_bindings(object.get("env"))?,
            }),
            "git_status" => Ok(Self::GitStatus),
            "read_process_output" => {
                let artifact_id = required_string(object, "artifact_id")?;
                let stream = match object.get("stream") {
                    None | Some(Value::Null) => CaptureStream::Stdout,
                    Some(Value::String(value)) if value == "stdout" => CaptureStream::Stdout,
                    Some(Value::String(value)) if value == "stderr" => CaptureStream::Stderr,
                    _ => {
                        return Err(harness_types::HarnessError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "provider read_process_output stream must be stdout or stderr",
                        ));
                    }
                };
                let offset = match object.get("offset") {
                    None | Some(Value::Null) => 0,
                    Some(value) => value.as_u64().ok_or_else(|| {
                        harness_types::HarnessError::new(
                            harness_types::ErrorCode::InvalidPayload,
                            "provider read_process_output offset must be a non-negative integer",
                        )
                    })?,
                };
                let length = match object.get("length") {
                    None | Some(Value::Null) => PROCESS_OUTPUT_PAGE_DEFAULT_BYTES,
                    Some(value) => value
                        .as_u64()
                        .and_then(|length| u32::try_from(length).ok())
                        .filter(|length| (1..=PROCESS_OUTPUT_PAGE_MAX_BYTES).contains(length))
                        .ok_or_else(|| {
                            harness_types::HarnessError::new(
                                harness_types::ErrorCode::InvalidPayload,
                                format!(
                                    "provider read_process_output length must be an integer between 1 and {PROCESS_OUTPUT_PAGE_MAX_BYTES}"
                                ),
                            )
                        })?,
                };
                Ok(Self::ReadProcessOutput {
                    artifact_id,
                    stream,
                    offset,
                    length,
                })
            }
            "git_diff" => Ok(Self::GitDiff {
                path: object
                    .get("path")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            }),
            "git_log" => {
                let limit = match object.get("limit") {
                    None | Some(Value::Null) => None,
                    Some(value) => Some(
                        value
                            .as_u64()
                            .and_then(|limit| u32::try_from(limit).ok())
                            .filter(|limit| (1..=GIT_LOG_MAX_LIMIT).contains(limit))
                            .ok_or_else(|| {
                                harness_types::HarnessError::new(
                                    harness_types::ErrorCode::InvalidPayload,
                                    format!(
                                        "provider git_log limit must be an integer between 1 and {GIT_LOG_MAX_LIMIT}"
                                    ),
                                )
                            })?,
                    ),
                };
                Ok(Self::GitLog {
                    path: object
                        .get("path")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    limit,
                })
            }
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

/// Parse the optional `env` object of a process call. It is a name → reference
/// map; a non-string or malformed reference never reaches the gate.
fn parse_env_bindings(
    value: Option<&Value>,
) -> Result<Vec<EnvBinding>, harness_types::HarnessError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let object = value.as_object().ok_or_else(|| {
        harness_types::HarnessError::new(
            harness_types::ErrorCode::EnvironmentDenied,
            "provider process env must be an object of secret references",
        )
    })?;
    if object.len() > MAX_ENV_BINDINGS {
        return Err(harness_types::HarnessError::new(
            harness_types::ErrorCode::EnvironmentDenied,
            format!("provider process env may name at most {MAX_ENV_BINDINGS} variables"),
        ));
    }
    let mut bindings = Vec::with_capacity(object.len());
    for (name, reference) in object {
        let reference = reference.as_str().ok_or_else(|| {
            harness_types::HarnessError::new(
                harness_types::ErrorCode::EnvironmentDenied,
                "provider process env values must be secret:// references, never literal values",
            )
        })?;
        bindings.push(EnvBinding {
            name: name.clone(),
            reference: reference.to_owned(),
        });
    }
    normalize_env_bindings(&bindings)
}

fn parse_isolation(value: Option<&Value>) -> Result<IsolationMode, harness_types::HarnessError> {
    match value {
        None => Ok(IsolationMode::BestEffort),
        Some(Value::String(value)) if value == "best_effort" => Ok(IsolationMode::BestEffort),
        Some(Value::String(value)) if value == "strict" => Ok(IsolationMode::Strict),
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
    /// The provider `call_id` this proposal came from, when it came from a
    /// model call. Correlation only: the host invocation id stays the authority.
    pub call_id: Option<String>,
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
            call_id: None,
            workspace_root: workspace_root.into(),
            action,
        }
    }

    /// Attach the provider call identity this proposal answers.
    #[must_use]
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }
}

/// How an advertised tool changes the world. The gate uses it to decide what an
/// approval must cover; it is part of the tool's durable descriptor, not a
/// property the model may choose.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    /// Reads only; never writes the workspace.
    ReadOnly,
    /// Writes inside the workspace root.
    Mutating,
    /// Starts a process or an external tool with host privileges.
    External,
}

impl EffectClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Mutating => "mutating",
            Self::External => "external",
        }
    }
}

/// One advertised coding tool, with the revision and schema digest the gate
/// validates a proposal against.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolDescriptor {
    pub id: String,
    pub revision: u32,
    /// Hash of the tool's provider-facing schema.
    pub schema_digest: String,
    pub effect_class: EffectClass,
    pub capabilities: Vec<String>,
}

/// The durable descriptors of every built-in coding tool.
///
/// The schema digest is computed from the same schema `coding_tool_schemas`
/// hands to the provider, so a schema change without a descriptor revision is
/// visible in the digest.
#[must_use]
pub fn coding_tool_descriptors() -> Vec<ToolDescriptor> {
    let schemas = coding_tool_schemas();
    let mut descriptors = coding_tool_names()
        .iter()
        .filter_map(|name| {
            let schema = schemas.iter().find(|schema| {
                schema
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(serde_json::Value::as_str)
                    == Some(*name)
            })?;
            let effect = effect_class_for(name);
            let capabilities = match effect {
                EffectClass::ReadOnly => vec!["workspace.read".to_owned()],
                EffectClass::Mutating => {
                    vec!["workspace.read".to_owned(), "workspace.write".to_owned()]
                }
                EffectClass::External => vec![
                    "workspace.read".to_owned(),
                    "process.spawn".to_owned(),
                    "network.none".to_owned(),
                ],
            };
            Some(ToolDescriptor {
                id: (*name).to_owned(),
                revision: u32::from(TOOL_CONTRACT_VERSION),
                schema_digest: ContentHash::from_canonical_json(schema)
                    .map(|hash| hash.as_str().to_owned())
                    .unwrap_or_default(),
                effect_class: effect,
                capabilities,
            })
        })
        .collect::<Vec<_>>();
    descriptors.sort_by(|left, right| left.id.cmp(&right.id));
    descriptors
}

/// The declared effect class of one built-in tool name.
#[must_use]
pub fn effect_class_for(name: &str) -> EffectClass {
    match name {
        "read_file"
        | "list_files"
        | "search_text"
        | "git_status"
        | "git_diff"
        | "git_log"
        | "read_process_output" => EffectClass::ReadOnly,
        "apply_patch" => EffectClass::Mutating,
        _ => EffectClass::External,
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
    pub(crate) session_id: harness_types::SessionId,
    pub(crate) task_id: harness_types::TaskId,
    pub(crate) invocation_id: harness_types::ToolInvocationId,
    pub(crate) call_id: Option<String>,
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

    #[must_use]
    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
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
        /// Whether the call waited behind another process for the host permit.
        queued: bool,
        /// True only when the backend confirmed the whole tree is gone; an
        /// unconfirmed reap is never reported as a settled success.
        tree_cleanup_confirmed: bool,
        /// How that confirmation was obtained: `nothing_to_clean`,
        /// `reaped_on_exit` or `killed_and_reaped`. A bare boolean cannot tell
        /// an operator whether a kill happened at all.
        tree_cleanup: String,
        /// Head preview of stdout, bounded by the spool's head limit.
        stdout: String,
        stderr: String,
        stdout_truncated: bool,
        stderr_truncated: bool,
        /// The durable capture of both streams, when one was published. It is
        /// what `read_process_output` pages through; `stdout`/`stderr` above are
        /// only the head of it.
        artifact_id: Option<String>,
        /// Bytes actually captured in that artifact.
        captured_bytes: u64,
        /// Digest of exactly those bytes.
        capture_hash: Option<ContentHash>,
        /// Whether the capture stopped at the quota before the stream ended.
        capture_truncated: bool,
        /// Tail preview of the capture, so a long log still shows how it ended.
        capture_tail: String,
    },
    /// One page of a captured process output.
    ProcessOutput {
        artifact_id: String,
        stream: String,
        offset: u64,
        /// Bytes in this page.
        length: u64,
        /// Bytes captured for this stream in total.
        total_bytes: u64,
        text: String,
    },
    Git {
        operation: String,
        output: String,
        truncated: bool,
    },
    TaskUpdate {
        note: String,
    },
    /// Result of a trusted external extension tool. The payload is plugin data;
    /// it is recorded as evidence, never interpreted as host authority.
    ExternalTool {
        plugin_id: String,
        tool_name: String,
        payload: Value,
        inflight: u64,
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
