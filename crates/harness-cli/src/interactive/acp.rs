//! prime-agent's ACP mode (`--mode acp`, `pa-daemon/src/acp`): the Agent
//! Client Protocol over stdio, line-delimited JSON-RPC 2.0, for editors that
//! host an agent (Zed and the like).
//!
//! Served: `initialize`, `session/new` (one session per connection, as
//! prime-agent hosts it, opened in the `cwd` it names, with the client's
//! `mcpServers`), `session/prompt` (text, images, embedded resources and
//! resource links), `session/set_config_option` (prime's `model` and
//! `thought_level` pickers), the `session/cancel` notification and
//! `session/close`. While a prompt runs, `session/update` notifications
//! stream `agent_message_chunk`, `agent_thought_chunk`, `tool_call` and
//! `tool_call_update`, plus `session_info_update` for goals and refinements
//! and `config_option_update` when the pickers change; the prompt is
//! answered with its `stopReason`. Nobody answers an approval here, so a
//! gated action is refused, as `ha exec` refuses it.

use std::collections::{BTreeMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use harness_types::McpServerConfigV2;

use harness_types::{HarnessError, InputId};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::config::ConfigOverrides;
use super::events::{RunOutcome, SessionEvent};
use super::service::{ApprovalDecision, SessionPort, SubmitRequest};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

fn response(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: &Value, code: i64, message: &str, data: Option<&Value>) -> Value {
    let error = match data {
        Some(data) => json!({ "code": code, "message": message, "data": data }),
        None => json!({ "code": code, "message": message }),
    };
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn notification(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// One incoming frame: a request (with an id) or a notification.
#[derive(Debug, PartialEq)]
enum Incoming {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

/// prime-agent's `parse_line`: `Err` is the error response to write back.
fn parse_line(line: &str) -> Result<Incoming, Value> {
    let bad_request = || error_response(&Value::Null, INVALID_REQUEST, "Invalid Request", None);
    let value: Value = serde_json::from_str(line.trim())
        .map_err(|_| error_response(&Value::Null, PARSE_ERROR, "Parse error", None))?;
    let Value::Object(object) = value else {
        return Err(bad_request());
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(bad_request());
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Err(bad_request());
    };
    let params = object.get("params").cloned().unwrap_or_else(|| json!({}));
    Ok(match object.get("id") {
        Some(Value::Null) | None => Incoming::Notification {
            method: method.to_owned(),
            params,
        },
        Some(id) => Incoming::Request {
            id: id.clone(),
            method: method.to_owned(),
            params,
        },
    })
}

/// prime-agent's `PromptBlockError::InvalidImage`.
const INVALID_IMAGE: &str = "image block requires base64 `data` and `mimeType` strings";

/// An image block of a prompt: base64 data and its type.
#[derive(Debug, PartialEq, Eq)]
struct ImageBlock {
    data: String,
    mime_type: String,
}

/// prime-agent's `parse_prompt_blocks`: the text (text blocks, embedded
/// resources as their uri line then text, resource links as their uri,
/// joined by newlines) and the image blocks.
fn prompt_blocks(prompt: &[Value]) -> Result<(String, Vec<ImageBlock>), &'static str> {
    let mut images = Vec::new();
    for block in prompt {
        if block.get("type").and_then(Value::as_str) == Some("image") {
            match (
                block.get("data").and_then(Value::as_str),
                block.get("mimeType").and_then(Value::as_str),
            ) {
                (Some(data), Some(mime_type)) => images.push(ImageBlock {
                    data: data.to_owned(),
                    mime_type: mime_type.to_owned(),
                }),
                _ => return Err(INVALID_IMAGE),
            }
        }
    }
    Ok((prompt_text(prompt), images))
}

/// Keep an ACP image beside the pasted ones (`<data>/attachments`) and name
/// it in the message, the way a pasted screenshot reaches the model.
fn save_image(directory: &Path, image: &ImageBlock) -> Result<PathBuf, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(image.data.trim())
        .map_err(|_| INVALID_IMAGE.to_owned())?;
    let extension = match image.mime_type.as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    };
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let path = directory.join(format!("acp-{}.{extension}", InputId::generate().as_str()));
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    Ok(path)
}

/// prime-agent's text half of `parse_prompt_blocks`.
fn prompt_text(prompt: &[Value]) -> String {
    let mut texts = Vec::new();
    for block in prompt {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    texts.push(text.to_owned());
                }
            }
            Some("resource") => {
                let resource = block.get("resource");
                let text = resource.and_then(|r| r.get("text")).and_then(Value::as_str);
                let uri = resource.and_then(|r| r.get("uri")).and_then(Value::as_str);
                if let Some(text) = text {
                    let uri = uri.map(|uri| format!("{uri}\n")).unwrap_or_default();
                    texts.push(format!("{uri}{text}"));
                }
            }
            Some("resource_link") => {
                if let Some(uri) = block.get("uri").and_then(Value::as_str) {
                    texts.push(uri.to_owned());
                }
            }
            _ => {}
        }
    }
    texts.join("\n")
}

/// prime-agent's tool kind map, over ha's tool names.
fn tool_kind(name: &str) -> &'static str {
    match name {
        "ipython" | "bash" | "run_shell" | "run_process" => "execute",
        "read_file" | "read" => "read",
        "edit_file" | "write_file" | "apply_patch" | "edit" | "write" => "edit",
        "search_text" | "glob" | "list_files" => "search",
        "web_fetch" | "web_search" => "fetch",
        _ => "other",
    }
}

/// The ACP stop reason of a finished run.
fn stop_reason(outcome: &RunOutcome) -> Result<&'static str, String> {
    match outcome {
        RunOutcome::Canceled => Ok("cancelled"),
        RunOutcome::Paused(_) => Ok("max_turn_requests"),
        RunOutcome::Failed(reason) => Err(reason.clone()),
        _ => Ok("end_turn"),
    }
}

/// prime-agent's `SERVER_NAME_PATTERN` (`/^[A-Za-z0-9][A-Za-z0-9_-]{0,max-1}$/`).
fn name_matches(name: &str, max: usize) -> bool {
    let mut characters = name.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && characters.clone().count() < max
        && characters.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The `name`/`value` pairs of an entry; one malformed item drops the entry.
fn pair_list(object: &serde_json::Map<String, Value>, key: &str) -> Option<Vec<(String, String)>> {
    object
        .get(key)?
        .as_array()?
        .iter()
        .map(|entry| {
            Some((
                entry.get("name")?.as_str()?.to_owned(),
                entry.get("value")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// prime-agent's `entries`: duplicates (headers case-insensitively) and
/// malformed names or values refuse the server.
fn checked_pairs(
    server: &str,
    label: &str,
    values: Vec<(String, String)>,
) -> Result<BTreeMap<String, String>, String> {
    let mut seen = HashSet::new();
    let mut result = BTreeMap::new();
    for (name, value) in values {
        if name.is_empty() {
            return Err(format!("MCP server {server} has an empty {label} name"));
        }
        let identity = if label == "header" {
            name.to_lowercase()
        } else {
            name.clone()
        };
        if !seen.insert(identity) {
            return Err(format!("MCP server {server} has duplicate {label} {name}"));
        }
        if label == "header" {
            let token = name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c));
            let printable = value
                .chars()
                .all(|c| c == '\t' || (' '..='~').contains(&c) || c as u32 >= 0x80);
            if !token || !printable {
                return Err(format!("MCP server {server} has an invalid HTTP header"));
            }
        } else if name.contains('=') || name.contains('\0') || value.contains('\0') {
            return Err(format!(
                "MCP server {server} has an invalid environment entry"
            ));
        }
        result.insert(name, value);
    }
    Ok(result)
}

/// An http(s) URL without embedded credentials (prime's `validate_http_url`).
fn http_url_ok(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    let scheme = scheme.to_lowercase();
    let rest = rest.trim_start_matches("//");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    (scheme == "http" || scheme == "https") && !authority.contains('@')
}

/// prime-agent's `resolve_acp_mcp_servers` and `acp_mcp_tool_names`: the
/// client's `mcpServers`, as ha's MCP server configuration. `Err` carries
/// the code and the message prime answers with.
fn admit_mcp_servers(
    servers: &[Value],
    cwd: &Path,
) -> Result<BTreeMap<String, McpServerConfigV2>, (i64, String)> {
    let invalid = |reason: String| (INVALID_PARAMS, reason);
    let mut admitted = BTreeMap::new();
    for server in servers {
        // Entries that do not match the ACP `McpServer` union are dropped.
        let Some(object) = server.as_object() else {
            continue;
        };
        let Some(name) = object.get("name").and_then(Value::as_str) else {
            continue;
        };
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let config = match kind {
            "http" | "sse" | "acp" => {
                let shape_ok = if kind == "acp" {
                    object.get("serverId").and_then(Value::as_str).is_some()
                } else {
                    object.get("url").and_then(Value::as_str).is_some()
                        && pair_list(object, "headers").is_some()
                };
                if !shape_ok {
                    continue;
                }
                if kind != "http" {
                    if !name_matches(name, 64) {
                        return Err(invalid(SERVER_NAME_RULE.to_owned()));
                    }
                    return Err(invalid(format!(
                        "MCP server {name} uses unsupported {kind} transport"
                    )));
                }
                None
            }
            _ => Some(()),
        };
        if !name_matches(name, 64) {
            return Err(invalid(SERVER_NAME_RULE.to_owned()));
        }
        if admitted.contains_key(name) {
            return Err(invalid(format!("duplicate MCP server name: {name}")));
        }
        let entry = if config.is_none() {
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !http_url_ok(url) {
                return Err(invalid(format!(
                    "MCP server {name} must use an HTTP(S) URL without embedded credentials"
                )));
            }
            let headers = checked_pairs(
                name,
                "header",
                pair_list(object, "headers").unwrap_or_default(),
            )
            .map_err(invalid)?;
            McpServerConfigV2 {
                transport: Some("streamable_http".to_owned()),
                url: Some(url.to_owned()),
                headers,
                ..McpServerConfigV2::default()
            }
        } else {
            let (Some(command), Some(args), Some(env)) = (
                object.get("command").and_then(Value::as_str),
                object
                    .get("args")
                    .and_then(Value::as_array)
                    .and_then(|args| {
                        args.iter()
                            .map(|arg| arg.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    }),
                pair_list(object, "env"),
            ) else {
                continue;
            };
            if command.is_empty() {
                return Err(invalid(format!("MCP server {name} has no stdio command")));
            }
            if command.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
                return Err(invalid(format!(
                    "MCP server {name} has an invalid stdio command"
                )));
            }
            McpServerConfigV2 {
                transport: Some("stdio".to_owned()),
                command: Some(command.to_owned()),
                args,
                env: checked_pairs(name, "environment", env).map_err(invalid)?,
                cwd: Some(cwd.display().to_string()),
                ..McpServerConfigV2::default()
            }
        };
        admitted.insert(name.to_owned(), entry);
    }
    // prime's tool-name check: `mcp_list_tools_<name>` stays within 64.
    if let Some(name) = admitted.keys().find(|name| !name_matches(name, 48)) {
        return Err((
            INTERNAL_ERROR,
            format!("Invalid ACP MCP server name: {name}"),
        ));
    }
    Ok(admitted)
}

const SERVER_NAME_RULE: &str = "MCP server names must start with an alphanumeric character and contain at most 64 alphanumeric, underscore, or hyphen characters";

/// prime-agent's opaque model value: the serialized `[provider, model-id]`.
fn model_value(provider: &str, model: &str) -> String {
    json!([provider, model]).to_string()
}

/// prime-agent's `session_config_options`: a `model` select over the models
/// that can be called (the current one always selectable) and, when the
/// model has levels other than `off`, a `thought_level` select.
fn config_options(service: &dyn SessionPort) -> Vec<Value> {
    let Some((provider, model)) = service.current_model() else {
        return Vec::new();
    };
    let mut available: Vec<(String, String)> = Vec::new();
    for (reference, name) in service.model_options() {
        let (option_provider, option_model) = reference.split_once('/').unwrap_or(("", &reference));
        let value = model_value(option_provider, option_model);
        let label = format!("{name} ({option_provider})");
        match available.iter_mut().find(|(key, _)| *key == value) {
            Some(slot) => slot.1 = label,
            None => available.push((value, label)),
        }
    }
    let current = model_value(&provider, &model);
    if !available.iter().any(|(value, _)| *value == current) {
        available.push((current.clone(), format!("{model} ({provider})")));
    }
    let mut options = vec![json!({
        "id": "model",
        "name": "Model",
        "type": "select",
        "category": "model",
        "currentValue": current,
        "options": available
            .iter()
            .map(|(value, name)| json!({ "value": value, "name": name }))
            .collect::<Vec<_>>(),
    })];
    let levels = service.thinking_levels();
    if levels.iter().any(|level| level != "off") {
        options.push(json!({
            "id": "thought_level",
            "name": "Reasoning effort",
            "type": "select",
            "category": "thought_level",
            "currentValue": service.thinking_level().unwrap_or_else(|| "off".to_owned()),
            "options": levels
                .iter()
                .map(|level| json!({ "value": level, "name": level }))
                .collect::<Vec<_>>(),
        }));
    }
    options
}

struct Session {
    id: String,
    service: Box<dyn SessionPort>,
    /// Where the prompt's images are kept.
    attachments: PathBuf,
    /// The picker options last published.
    published: Vec<Value>,
    /// The `session/prompt` waiting for its run to end.
    prompt: Option<Value>,
    message: Option<String>,
    messages: u64,
    tool_output: Option<String>,
    last_error: Option<String>,
}

struct Acp {
    session: Option<Session>,
    events: Option<mpsc::UnboundedReceiver<SessionEvent>>,
    overrides: ConfigOverrides,
    out: Vec<Value>,
}

impl Acp {
    fn update(&mut self, update: &Value) {
        let Some(session) = &self.session else {
            return;
        };
        let params = json!({ "sessionId": session.id, "update": update });
        self.out.push(notification("session/update", &params));
    }

    fn request(&mut self, id: Value, method: &str, params: &Value) {
        match method {
            "initialize" => {
                if !params.get("protocolVersion").is_some_and(Value::is_number) {
                    self.out.push(error_response(
                        &id,
                        INVALID_PARAMS,
                        "Invalid params",
                        Some(&json!({ "protocolVersion": "expected a number" })),
                    ));
                    return;
                }
                self.out.push(response(
                    &id,
                    &json!({
                        "protocolVersion": 1,
                        "agentCapabilities": {
                            "loadSession": false,
                            "promptCapabilities": { "image": true, "embeddedContext": true },
                            "mcpCapabilities": { "http": true },
                            "sessionCapabilities": { "close": {} },
                        },
                        "agentInfo": {
                            "name": "ha",
                            "title": "harness-agents",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }),
                ));
            }
            "session/new" => self.new_session(&id, params),
            "session/set_config_option" => self.set_config_option(&id, params),
            "session/prompt" => self.prompt(id, params),
            "session/close" => {
                let known = self.session.as_ref().is_some_and(|session| {
                    params.get("sessionId").and_then(Value::as_str) == Some(session.id.as_str())
                });
                if known {
                    if let Some(session) = &mut self.session {
                        session.service.cancel();
                    }
                    self.out.push(response(&id, &json!({})));
                } else {
                    self.out.push(error_response(
                        &id,
                        INTERNAL_ERROR,
                        &format!(
                            "Unknown ACP session: {}",
                            params
                                .get("sessionId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                        ),
                        None,
                    ));
                }
            }
            other => self.out.push(error_response(
                &id,
                METHOD_NOT_FOUND,
                &format!("\"Method not found\": {other}"),
                Some(&json!({ "method": other })),
            )),
        }
    }

    fn new_session(&mut self, id: &Value, params: &Value) {
        if self.session.is_some() {
            self.out.push(error_response(
                id,
                INTERNAL_ERROR,
                "ha ACP mode hosts one session per connection; start another ha process for a second session",
                None,
            ));
            return;
        }
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from);
        let environment = super::paths::LaunchEnvironment::capture();
        let context = std::env::current_dir()
            .map_err(|error| error.to_string())
            .and_then(|caller_dir| {
                super::bootstrap::resolve(super::bootstrap::LaunchRequest {
                    cwd,
                    caller_dir,
                    platform: super::paths::HostPlatform::current(),
                    environment: environment.clone(),
                    explicit_data_dir: None,
                })
                .map_err(|error| error.to_string())
            });
        let context = match context {
            Ok(context) => context,
            Err(message) => {
                self.out
                    .push(error_response(id, INTERNAL_ERROR, &message, None));
                return;
            }
        };
        let servers = params
            .get("mcpServers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mcp_servers = match admit_mcp_servers(&servers, &context.project.root) {
            Ok(servers) => servers,
            Err((code, message)) => {
                let frame = if code == INVALID_PARAMS {
                    error_response(
                        id,
                        INVALID_PARAMS,
                        "Invalid params",
                        Some(&json!({ "reason": message })),
                    )
                } else {
                    error_response(id, code, &message, None)
                };
                self.out.push(frame);
                return;
            }
        };
        let mut overrides = self.overrides.clone();
        overrides.mcp_servers = mcp_servers;
        let (sender, events) = mpsc::unbounded_channel();
        let service = super::service::AgentSessionService::new_with_overrides(
            &context,
            environment,
            sender,
            overrides,
        );
        let session_id = service
            .conversation_id()
            .unwrap_or_else(|| InputId::generate().as_str().to_owned());
        let published = config_options(&service);
        self.out.push(response(
            id,
            &json!({ "sessionId": session_id, "configOptions": published }),
        ));
        self.events = Some(events);
        self.session = Some(Session {
            id: session_id,
            service: Box::new(service),
            attachments: context.paths.data_dir.join("attachments"),
            published,
            prompt: None,
            message: None,
            messages: 0,
            tool_output: None,
            last_error: None,
        });
    }

    fn prompt(&mut self, id: Value, params: &Value) {
        let requested = params.get("sessionId").and_then(Value::as_str);
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| Some(session.id.as_str()) == requested)
        else {
            let message = format!("Unknown ACP session: {}", requested.unwrap_or_default());
            self.out
                .push(error_response(&id, INTERNAL_ERROR, &message, None));
            return;
        };
        if session.prompt.is_some() {
            self.out.push(error_response(
                &id,
                INTERNAL_ERROR,
                "A prompt is already running in this session",
                None,
            ));
            return;
        }
        let blocks = params
            .get("prompt")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let (mut text, images) = match prompt_blocks(&blocks) {
            Ok(parsed) => parsed,
            Err(reason) => {
                self.out.push(error_response(
                    &id,
                    INVALID_PARAMS,
                    "Invalid params",
                    Some(&json!({ "reason": reason })),
                ));
                return;
            }
        };
        for image in &images {
            match save_image(&session.attachments, image) {
                Ok(path) => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("\"{}\"", path.display()));
                }
                Err(reason) => {
                    self.out.push(error_response(
                        &id,
                        INVALID_PARAMS,
                        "Invalid params",
                        Some(&json!({ "reason": reason })),
                    ));
                    return;
                }
            }
        }
        if text.trim().is_empty() {
            self.out.push(error_response(
                &id,
                INVALID_PARAMS,
                "Invalid params",
                Some(&json!({ "reason": "the prompt has no text" })),
            ));
            return;
        }
        session.prompt = Some(id);
        session.message = None;
        session.last_error = None;
        session.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text,
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
    }

    /// prime-agent's `session/set_config_option`: apply the `model` or
    /// `thought_level` selection and answer the refreshed options.
    fn set_config_option(&mut self, id: &Value, params: &Value) {
        let requested = params
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let invalid = |reason: String| {
            error_response(
                id,
                INVALID_PARAMS,
                "Invalid params",
                Some(&json!({ "reason": reason })),
            )
        };
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.id == requested)
        else {
            self.out
                .push(invalid(format!("Unknown ACP session: {requested}")));
            return;
        };
        let config_id = params
            .get("configId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = params.get("value").and_then(Value::as_str);
        let applied = match (config_id, value) {
            ("model", Some(value)) => {
                let current = session
                    .service
                    .current_model()
                    .map(|(provider, model)| model_value(&provider, &model));
                if current.as_deref() == Some(value) {
                    Ok(())
                } else {
                    let chosen =
                        session
                            .service
                            .model_options()
                            .into_iter()
                            .find(|(reference, _)| {
                                let (provider, model) =
                                    reference.split_once('/').unwrap_or(("", reference));
                                model_value(provider, model) == value
                            });
                    match chosen {
                        None => Err(invalid(format!("Unavailable model: {value}"))),
                        Some((reference, _)) => session
                            .service
                            .set_model(&reference)
                            .map(|_| ())
                            .map_err(|message| {
                                error_response(
                                    id,
                                    INTERNAL_ERROR,
                                    "Internal error",
                                    Some(&json!({ "details": message })),
                                )
                            }),
                    }
                }
            }
            ("thought_level", Some(value)) => {
                let levels = session.service.thinking_levels();
                if levels.iter().any(|level| level != "off")
                    && levels.iter().any(|level| level == value)
                {
                    session
                        .service
                        .set_thinking(value)
                        .map(|_| ())
                        .map_err(|message| {
                            error_response(
                                id,
                                INTERNAL_ERROR,
                                "Internal error",
                                Some(&json!({ "details": message })),
                            )
                        })
                } else {
                    Err(invalid(format!("Unsupported reasoning effort: {value}")))
                }
            }
            (other, _) => Err(invalid(format!("Unknown config option: {other}"))),
        };
        if let Err(frame) = applied {
            self.out.push(frame);
            return;
        }
        self.refresh_options();
        let options = self
            .session
            .as_ref()
            .map(|session| session.published.clone())
            .unwrap_or_default();
        self.out
            .push(response(id, &json!({ "configOptions": options })));
    }

    /// prime-agent's `refreshConfig`: publish `config_option_update` when the
    /// options changed.
    fn refresh_options(&mut self) {
        let Some(session) = &mut self.session else {
            return;
        };
        let options = config_options(session.service.as_ref());
        if options == session.published {
            return;
        }
        session.published.clone_from(&options);
        self.update(&json!({
            "sessionUpdate": "config_option_update",
            "configOptions": options,
        }));
    }

    /// prime-agent's `session_info_update`, under its `_meta` namespace.
    fn info_update(&mut self, meta: Value) {
        self.update(&json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "ai.primeintellect.prime-agent": meta },
        }));
    }

    fn notification(&mut self, method: &str, params: &Value) {
        if method == "session/cancel"
            && let Some(session) = &mut self.session
            && params.get("sessionId").and_then(Value::as_str) == Some(session.id.as_str())
        {
            session.service.cancel();
        }
    }

    /// The id of the assistant message in progress, opened on first use.
    fn message_id(&mut self) -> String {
        let Some(session) = &mut self.session else {
            return String::new();
        };
        if session.message.is_none() {
            session.messages += 1;
            session.message = Some(format!("{}-{}", session.id, session.messages));
        }
        session.message.clone().unwrap_or_default()
    }

    fn chunk(&mut self, kind: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        let message_id = self.message_id();
        self.update(&json!({
            "sessionUpdate": kind,
            "messageId": message_id,
            "content": { "type": "text", "text": text },
        }));
    }

    fn event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::TextDelta { text } => self.chunk("agent_message_chunk", &text),
            SessionEvent::ThinkingDelta { text } => self.chunk("agent_thought_chunk", &text),
            SessionEvent::ToolStarted {
                name,
                call_id,
                input,
                ..
            } => {
                if let Some(session) = &mut self.session {
                    session.message = None;
                }
                let args = serde_json::from_str::<Value>(&input).unwrap_or(json!(input));
                let (title, raw_input) = if name == "ipython" {
                    let code = args.get("code").cloned().unwrap_or(Value::Null);
                    ("Python cell".to_owned(), json!({ "code": code }))
                } else {
                    (name.clone(), args)
                };
                self.update(&json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": call_id,
                    "title": title,
                    "kind": tool_kind(&name),
                    "status": "in_progress",
                    "rawInput": raw_input,
                }));
            }
            SessionEvent::ToolOutput { text } => {
                if let Some(session) = &mut self.session {
                    session.tool_output = Some(text);
                }
            }
            SessionEvent::ToolSettled {
                call_id,
                ok,
                detail,
                ..
            } => {
                let output = self
                    .session
                    .as_mut()
                    .and_then(|session| session.tool_output.take())
                    .unwrap_or(detail);
                let mut update = json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": call_id,
                    "status": if ok { "completed" } else { "failed" },
                });
                if !output.is_empty() {
                    update["content"] = json!([
                        { "type": "content", "content": { "type": "text", "text": output } }
                    ]);
                }
                self.update(&update);
            }
            SessionEvent::ApprovalRequired { request_id, .. } => {
                if let Some(session) = &mut self.session {
                    let _ = session
                        .service
                        .answer(&request_id, ApprovalDecision::Denied);
                }
            }
            SessionEvent::GoalCreated {
                objective,
                token_budget,
            } => {
                let mut goal = json!({ "status": "active", "objective": objective });
                if let Some(budget) = token_budget {
                    goal["tokenBudget"] = json!(budget);
                }
                self.info_update(json!({ "goal": goal }));
            }
            SessionEvent::GoalCompleted { .. } => {
                self.info_update(json!({ "goal": { "status": "complete" } }));
            }
            SessionEvent::Refined {
                summary, details, ..
            } => {
                self.info_update(json!({ "refinement": {
                    "status": "complete",
                    "summary": summary,
                    "changes": details,
                }}));
            }
            // A turn that broke is over, as the app's controller ends it.
            SessionEvent::RecoverableError { message } => {
                self.finish(&RunOutcome::Failed(message));
            }
            SessionEvent::RunTerminal { outcome } => {
                self.finish(&outcome);
                // A turn can change the model (the backup) or its levels.
                self.refresh_options();
            }
            _ => {}
        }
    }

    fn finish(&mut self, outcome: &RunOutcome) {
        let Some(session) = &mut self.session else {
            return;
        };
        session.message = None;
        let Some(id) = session.prompt.take() else {
            return;
        };
        let frame = match stop_reason(outcome) {
            Ok(reason) => response(&id, &json!({ "stopReason": reason })),
            Err(reason) => {
                let message = session.last_error.take().unwrap_or(reason);
                error_response(&id, INTERNAL_ERROR, &message, None)
            }
        };
        self.out.push(frame);
    }

    fn running(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.prompt.is_some())
    }
}

/// Serve the ACP mode until stdin closes.
///
/// # Errors
/// Never: protocol failures are answered on stdout.
pub async fn run(overrides: ConfigOverrides) -> Result<ExitCode, HarnessError> {
    let (lines_in, mut lines) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines_in.send(line).is_err() {
                break;
            }
        }
    });
    let mut acp = Acp {
        session: None,
        events: None,
        overrides,
        out: Vec::new(),
    };
    let mut stdin_open = true;
    loop {
        let events = acp.events.as_mut();
        tokio::select! {
            line = lines.recv(), if stdin_open => match line {
                Some(line) if line.trim().is_empty() => {}
                Some(line) => match parse_line(&line) {
                    Ok(Incoming::Request { id, method, params }) => {
                        acp.request(id, &method, &params);
                    }
                    Ok(Incoming::Notification { method, params }) => {
                        acp.notification(&method, &params);
                    }
                    Err(frame) => acp.out.push(frame),
                },
                None => stdin_open = false,
            },
            Some(event) = async {
                match events {
                    Some(events) => events.recv().await,
                    None => std::future::pending().await,
                }
            } => acp.event(event),
        }
        for frame in acp.out.drain(..) {
            if let Ok(line) = serde_json::to_string(&frame) {
                println!("{line}");
            }
        }
        if !stdin_open && !acp.running() {
            return Ok(ExitCode::SUCCESS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INTERNAL_ERROR, INVALID_PARAMS, Incoming, admit_mcp_servers, parse_line, prompt_blocks,
        prompt_text, tool_kind,
    };
    use serde_json::json;

    #[test]
    fn mcp_servers_are_admitted_as_prime_admits_them() {
        let cwd = std::path::Path::new("/work");
        let admitted = admit_mcp_servers(
            &[
                json!({ "name": "files", "command": "mcp-files", "args": ["--ro"], "env": [{ "name": "LEVEL", "value": "1" }] }),
                json!({ "name": "web", "type": "http", "url": "https://mcp.example/x", "headers": [] }),
                json!({ "name": "broken" }),
            ],
            cwd,
        )
        .expect("admitted");
        assert_eq!(admitted.len(), 2);
        assert_eq!(admitted["files"].command.as_deref(), Some("mcp-files"));
        assert_eq!(
            admitted["web"].transport.as_deref(),
            Some("streamable_http")
        );
        assert_eq!(
            admit_mcp_servers(
                &[json!({ "name": "a", "type": "sse", "url": "https://x", "headers": [] })],
                cwd
            )
            .unwrap_err(),
            (
                INVALID_PARAMS,
                "MCP server a uses unsupported sse transport".to_owned()
            )
        );
        assert_eq!(
            admit_mcp_servers(
                &[json!({ "name": "a", "type": "http", "url": "https://u:p@x", "headers": [] })],
                cwd
            )
            .unwrap_err()
            .1,
            "MCP server a must use an HTTP(S) URL without embedded credentials"
        );
        let long = "a".repeat(50);
        assert_eq!(
            admit_mcp_servers(
                &[json!({ "name": long, "command": "c", "args": [], "env": [] })],
                cwd
            )
            .unwrap_err()
            .0,
            INTERNAL_ERROR
        );
    }

    #[test]
    fn prompt_images_need_data_and_a_type() {
        let (text, images) = prompt_blocks(&[
            json!({ "type": "text", "text": "what is this?" }),
            json!({ "type": "image", "data": "aGk=", "mimeType": "image/png" }),
        ])
        .expect("parsed");
        assert_eq!(text, "what is this?");
        assert_eq!(images.len(), 1);
        assert!(prompt_blocks(&[json!({ "type": "image", "data": "aGk=" })]).is_err());
    }

    #[test]
    fn frames_and_prompts_read_as_prime_reads_them() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}"#),
            Ok(Incoming::Notification {
                method: "session/cancel".to_owned(),
                params: json!({ "sessionId": "s" }),
            })
        );
        assert_eq!(
            parse_line("{nope").unwrap_err()["error"]["code"],
            json!(-32700)
        );
        assert_eq!(
            parse_line(r#"{"id":1,"method":"initialize"}"#).unwrap_err()["error"]["code"],
            json!(-32600)
        );
        assert_eq!(
            prompt_text(&[
                json!({ "type": "text", "text": "hello" }),
                json!({ "type": "resource", "resource": { "uri": "file:///x", "text": "body" } }),
                json!({ "type": "resource_link", "uri": "https://example" }),
            ]),
            "hello\nfile:///x\nbody\nhttps://example"
        );
        assert_eq!(tool_kind("ipython"), "execute");
        assert_eq!(tool_kind("edit_file"), "edit");
        assert_eq!(tool_kind("delegate"), "other");
    }
}
