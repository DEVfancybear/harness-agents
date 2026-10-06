//! prime-agent's ACP mode (`--mode acp`, `pa-daemon/src/acp`): the Agent
//! Client Protocol over stdio, line-delimited JSON-RPC 2.0, for editors that
//! host an agent (Zed and the like).
//!
//! Served: `initialize`, `session/new` (one session per connection, as
//! prime-agent hosts it, opened in the `cwd` it names), `session/prompt`
//! (text, embedded resources and resource links joined into the message),
//! the `session/cancel` notification and `session/close`. While a prompt
//! runs, `session/update` notifications stream `agent_message_chunk`,
//! `agent_thought_chunk`, `tool_call` and `tool_call_update`; the prompt is
//! answered with its `stopReason`. Nobody answers an approval here, so a
//! gated action is refused, as `ha exec` refuses it.

use std::io::BufRead;
use std::process::ExitCode;

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

/// prime-agent's `parse_prompt_blocks`: text blocks, embedded resources (uri
/// line, then text) and resource links (the uri), joined by newlines.
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

struct Session {
    id: String,
    service: Box<dyn SessionPort>,
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
                            "promptCapabilities": { "image": false, "embeddedContext": true },
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
        let (sender, events) = mpsc::unbounded_channel();
        let service = super::service::AgentSessionService::new_with_overrides(
            &context,
            environment,
            sender,
            self.overrides.clone(),
        );
        let session_id = service
            .conversation_id()
            .unwrap_or_else(|| InputId::generate().as_str().to_owned());
        self.out
            .push(response(id, &json!({ "sessionId": session_id })));
        self.events = Some(events);
        self.session = Some(Session {
            id: session_id,
            service: Box::new(service),
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
        let text = prompt_text(&blocks);
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
            // A turn that broke is over, as the app's controller ends it.
            SessionEvent::RecoverableError { message } => {
                self.finish(&RunOutcome::Failed(message));
            }
            SessionEvent::RunTerminal { outcome } => self.finish(&outcome),
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
    use super::{Incoming, parse_line, prompt_text, tool_kind};
    use serde_json::json;

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
