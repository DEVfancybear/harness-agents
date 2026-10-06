//! prime-agent's RPC mode (`--mode rpc`, `pa-daemon/src/rpc`, TS
//! `modes/rpc`): headless operation with JSON commands on stdin and JSON
//! responses and events on stdout, one per line.
//!
//! A command is `{"id"?, "type", ...}`; its answer is
//! `{"id"?, "type": "response", "command", "success", "data"?}` or the same
//! with `"success": false, "error"`. A line that is not an object with a
//! string `type` answers the `parse` error. The session's events follow as
//! prime-agent's `session_event` shapes: `agent_start`, `message_start`,
//! `message_update` (`text_delta` / `thinking_delta`), `message_end`,
//! `tool_execution_start` / `_update` / `_end` and `agent_end`.
//!
//! The commands are prime-agent's in-process RPC set: `prompt` (with
//! `streamingBehavior` `steer` or `followUp` while a turn runs), `steer`,
//! `follow_up`, `abort`, `new_session`, `get_state`, `set_model`,
//! `cycle_model`, `get_available_models`, `set_thinking_level`,
//! `cycle_thinking_level`, `set_steering_mode`, `set_follow_up_mode`,
//! `compact`, `refine`, `set_auto_compaction`, `set_auto_retry`,
//! `abort_retry`, `switch_session`, `fork`, `clone`, `get_fork_messages`,
//! `get_last_assistant_text`, `set_session_name`, `get_messages`,
//! `export_html`, `get_session_stats` and `get_commands`; the scheduling,
//! messaging, `observe` and `bash` commands answer as prime's in-process
//! transport answers them. Nobody can answer an approval here, so a gated
//! action is refused, as `ha exec` refuses it. Closing stdin lets the
//! running turn finish, then the process exits.

use std::collections::VecDeque;
use std::io::BufRead;
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError, InputId};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use super::config::ConfigOverrides;
use super::events::{RunOutcome, SessionEvent};
use super::service::{ApprovalDecision, SessionPort, SubmitRequest};

/// prime-agent's admission error for a prompt sent while a turn runs.
const BUSY: &str = "Agent is already processing. Specify streamingBehavior ('steer' or 'followUp') to queue the message.";

/// prime-agent's thinking-level wire names.
const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// prime-agent's in-process answers for the daemon-only commands.
const CRON_REQUIRES_DAEMON: &str = "Cron jobs require daemon mode";
const HEARTBEATS_REQUIRE_DAEMON: &str = "Heartbeats require daemon mode";
const AGENT_MESSAGING_REQUIRES_DAEMON: &str = "Agent messaging requires daemon mode";
const BASH_BACKEND_GAP: &str = "Bash execution requires the session bash executor, which is not linked into the in-process RPC transport yet; the daemon-attached RPC transport serves it";

/// A command whose answer waits for the session.
enum Pending {
    /// `get_fork_messages`: the conversation's turns.
    ForkMessages(Option<Value>),
    /// `fork`: the turn to fork before, once the turns are listed.
    Fork(Option<Value>, String),
    /// `get_messages`.
    Messages(Option<Value>),
    /// `get_session_stats`.
    Stats(Option<Value>),
    /// `export_html`.
    Export(Option<Value>),
}

/// A success response; `data` is left out when `None`.
#[must_use]
pub fn success(id: Option<&Value>, command: &str, data: Option<Value>) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".to_owned(), id.clone());
    }
    object.insert("type".to_owned(), json!("response"));
    object.insert("command".to_owned(), json!(command));
    object.insert("success".to_owned(), json!(true));
    if let Some(data) = data {
        object.insert("data".to_owned(), data);
    }
    Value::Object(object)
}

/// An error response.
#[must_use]
pub fn error(id: Option<&Value>, command: &str, message: &str) -> Value {
    let mut object = Map::new();
    if let Some(id) = id {
        object.insert("id".to_owned(), id.clone());
    }
    object.insert("type".to_owned(), json!("response"));
    object.insert("command".to_owned(), json!(command));
    object.insert("success".to_owned(), json!(false));
    object.insert("error".to_owned(), json!(message));
    Value::Object(object)
}

/// One stdin line: the command's id, type and payload, or the parse error
/// to answer with.
///
/// # Errors
/// The `parse` error response, when the line is not a command.
pub fn parse_line(line: &str) -> Result<(Option<Value>, String, Value), Value> {
    let parsed: Value = serde_json::from_str(line.trim()).map_err(|parse_error| {
        error(
            None,
            "parse",
            &format!("Failed to parse command: {parse_error}"),
        )
    })?;
    let invalid = || {
        error(
            None,
            "parse",
            "Invalid command: expected an object with a string type",
        )
    };
    let Value::Object(object) = &parsed else {
        return Err(invalid());
    };
    let Some(Value::String(command)) = object.get("type") else {
        return Err(invalid());
    };
    let id = object.get("id").cloned().filter(|id| !id.is_null());
    Ok((id, command.clone(), parsed.clone()))
}

/// The assistant message of the turn so far, in prime-agent's shape.
#[derive(Default)]
struct Message {
    started: bool,
    thinking: String,
    text: String,
}

impl Message {
    fn content(&self) -> Vec<Value> {
        let mut content = Vec::new();
        if !self.thinking.is_empty() {
            content.push(json!({ "type": "thinking", "thinking": self.thinking }));
        }
        if !self.text.is_empty() {
            content.push(json!({ "type": "text", "text": self.text }));
        }
        content
    }

    fn value(&self, stop_reason: Option<&str>, error_message: Option<&str>) -> Value {
        let mut message = json!({ "role": "assistant", "content": self.content() });
        if let Some(reason) = stop_reason {
            message["stopReason"] = json!(reason);
        }
        if let Some(text) = error_message {
            message["errorMessage"] = json!(text);
        }
        message
    }
}

/// The connection's state around the session.
struct Rpc {
    service: Box<dyn SessionPort>,
    running: bool,
    /// `abort` stops queued input from being delivered until the next
    /// prompt, steer or follow-up, as prime-agent's `requestAbort` does.
    paused: bool,
    steering: VecDeque<String>,
    follow_ups: VecDeque<String>,
    message: Message,
    /// The settled assistant messages of the running turn, for `agent_end`.
    messages: Vec<Value>,
    /// The running tool calls: id, name and arguments.
    tools: Vec<(String, String, Value)>,
    tool_output: Option<String>,
    last_answer: Option<String>,
    last_error: Option<String>,
    /// A `compact` waiting for its run to end: its id (a command may have none).
    #[allow(clippy::option_option)] // no compaction, or one with or without an id
    compacting: Option<Option<Value>>,
    /// A `compact` asked for while a turn ran: it starts when the turn ends.
    compact_next: Option<(Option<Value>, Option<String>)>,
    /// A `refine` waiting for its run to end, and what it applied.
    #[allow(clippy::option_option)] // no refinement, or one with or without an id
    refining: Option<Option<Value>>,
    refined: Option<Value>,
    /// Commands waiting for the session to read something.
    pending: Vec<Pending>,
    steering_mode: super::queue::QueueMode,
    follow_up_mode: super::queue::QueueMode,
    out: Vec<Value>,
}

impl Rpc {
    fn emit(&mut self, frame: Value) {
        self.out.push(frame);
    }

    fn start(&mut self, text: String) {
        self.paused = false;
        self.running = true;
        self.messages.clear();
        self.message = Message::default();
        self.last_error = None;
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text,
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: None,
            refine: None,
        });
        self.emit(json!({ "type": "agent_start" }));
    }

    fn start_compaction(&mut self, id: Option<Value>, guidance: Option<String>) {
        self.running = true;
        self.compacting = Some(id);
        self.service.submit(SubmitRequest {
            input_id: InputId::generate(),
            text: "/compact".to_owned(),
            answer_question_id: None,
            shell_prefix: None,
            compact_guidance: Some(guidance.unwrap_or_default()),
            refine: None,
        });
        self.emit(json!({ "type": "compaction_start", "reason": "manual" }));
    }

    /// End the assistant message in progress, if one started.
    fn end_message(&mut self, stop_reason: &str, error_message: Option<&str>) {
        if !self.message.started {
            return;
        }
        let message = self.message.value(Some(stop_reason), error_message);
        if !self.message.text.is_empty() {
            self.last_answer = Some(self.message.text.clone());
        }
        self.emit(json!({ "type": "message_end", "message": message }));
        self.messages.push(message);
        self.message = Message::default();
    }

    fn delta(&mut self, kind: &str, delta: &str) {
        if !self.message.started {
            self.message.started = true;
            let message = self.message.value(None, None);
            self.emit(json!({ "type": "message_start", "message": message }));
        }
        let index = if kind == "thinking_delta" {
            self.message.thinking.push_str(delta);
            0
        } else {
            self.message.text.push_str(delta);
            usize::from(!self.message.thinking.is_empty())
        };
        let message = self.message.value(None, None);
        self.emit(json!({
            "type": "message_update",
            "message": message,
            "assistantMessageEvent": { "type": kind, "contentIndex": index, "delta": delta },
        }));
    }

    fn event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::TextDelta { text } => self.delta("text_delta", &text),
            SessionEvent::ThinkingDelta { text } => self.delta("thinking_delta", &text),
            SessionEvent::StreamRestarted => self.message = Message::default(),
            SessionEvent::ToolStarted {
                name,
                call_id,
                input,
                ..
            } => {
                self.end_message("toolUse", None);
                let args = serde_json::from_str::<Value>(&input).unwrap_or(json!(input));
                self.emit(json!({
                    "type": "tool_execution_start",
                    "toolCallId": call_id,
                    "toolName": name,
                    "args": args,
                }));
                self.tools.push((call_id, name, args));
            }
            SessionEvent::ToolProgress { call_id, text } => {
                let Some((_, name, args)) = self.tools.iter().find(|(id, ..)| *id == call_id)
                else {
                    return;
                };
                let frame = json!({
                    "type": "tool_execution_update",
                    "toolCallId": call_id,
                    "toolName": name,
                    "args": args,
                    "partialResult": { "content": [{ "type": "text", "text": text }] },
                });
                self.emit(frame);
            }
            SessionEvent::ToolOutput { text } => self.tool_output = Some(text),
            SessionEvent::ToolSettled {
                name,
                call_id,
                ok,
                detail,
                ..
            } => {
                self.tools.retain(|(id, ..)| *id != call_id);
                let output = self.tool_output.take().unwrap_or(detail);
                self.emit(json!({
                    "type": "tool_execution_end",
                    "toolCallId": call_id,
                    "toolName": name,
                    "result": { "content": [{ "type": "text", "text": output }] },
                    "isError": !ok,
                }));
            }
            // Nobody can answer here: refused, as `ha exec` refuses.
            SessionEvent::ApprovalRequired { request_id, .. } => {
                let _ = self.service.answer(&request_id, ApprovalDecision::Denied);
            }
            SessionEvent::UnreadMessage {
                text,
                verbatim: false,
            } => self.steering.push_back(text),
            // A turn that broke is over, as the app's controller ends it; a
            // terminal event after it finds nothing running.
            SessionEvent::RecoverableError { message } if self.running => {
                self.last_error = Some(message.clone());
                self.finish(&RunOutcome::Failed(message));
            }
            SessionEvent::RunTerminal { outcome } if self.running => self.finish(&outcome),
            SessionEvent::Refined {
                header,
                summary,
                details,
            } if self.refining.is_some() => {
                self.refined = Some(json!({
                    "header": header,
                    "summary": summary,
                    "details": details,
                }));
            }
            SessionEvent::TurnsListed { turns, .. } => self.turns_listed(&turns),
            SessionEvent::ConversationRead { messages } => self.conversation_read(messages),
            SessionEvent::HtmlExported { result } => {
                if let Some(index) = self
                    .pending
                    .iter()
                    .position(|pending| matches!(pending, Pending::Export(_)))
                    && let Pending::Export(id) = self.pending.remove(index)
                {
                    let response = match result {
                        Ok(path) => {
                            success(id.as_ref(), "export_html", Some(json!({ "path": path })))
                        }
                        Err(message) => error(id.as_ref(), "export_html", &message),
                    };
                    self.emit(response);
                }
            }
            _ => {}
        }
    }

    /// The turns `get_fork_messages` or `fork` waited for.
    fn turns_listed(&mut self, turns: &[(String, String)]) {
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| matches!(pending, Pending::ForkMessages(_) | Pending::Fork(..)))
        else {
            return;
        };
        match self.pending.remove(index) {
            Pending::ForkMessages(id) => {
                // prime-agent's `getUserMessagesForForking`: the user
                // messages with text, in order.
                let messages = turns
                    .iter()
                    .filter(|(_, text)| !text.is_empty())
                    .map(|(entry, text)| json!({ "entryId": entry, "text": text }))
                    .collect::<Vec<_>>();
                self.emit(success(
                    id.as_ref(),
                    "get_fork_messages",
                    Some(json!({ "messages": messages })),
                ));
            }
            Pending::Fork(id, entry) => {
                let response = match turns.iter().find(|(session, _)| *session == entry) {
                    None => error(id.as_ref(), "fork", "Invalid entry ID for forking"),
                    Some((session, text)) => match self.service.fork(session, true) {
                        Ok(()) => success(
                            id.as_ref(),
                            "fork",
                            Some(json!({ "text": text, "cancelled": false })),
                        ),
                        Err(message) => error(id.as_ref(), "fork", &message),
                    },
                };
                self.emit(response);
            }
            _ => {}
        }
    }

    /// The messages `get_messages` or `get_session_stats` waited for.
    fn conversation_read(&mut self, messages: Vec<Value>) {
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| matches!(pending, Pending::Messages(_) | Pending::Stats(_)))
        else {
            return;
        };
        match self.pending.remove(index) {
            Pending::Messages(id) => {
                self.emit(success(
                    id.as_ref(),
                    "get_messages",
                    Some(json!({ "messages": messages })),
                ));
            }
            Pending::Stats(id) => {
                let stats = self.stats(&messages);
                self.emit(success(id.as_ref(), "get_session_stats", Some(stats)));
            }
            _ => {}
        }
    }

    /// prime-agent's `getSessionStats`, over the conversation's messages and
    /// the session's usage.
    fn stats(&self, messages: &[Value]) -> Value {
        let role = |name: &str| {
            messages
                .iter()
                .filter(|message| message["role"] == name)
                .count()
        };
        let tool_calls = messages
            .iter()
            .filter(|message| message["role"] == "assistant")
            .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
            .filter(|block| block["type"] == "toolCall")
            .count();
        let (input, output, cost) = self.service.session_usage();
        json!({
            "sessionFile": Value::Null,
            "sessionId": self.service.conversation_id(),
            "userMessages": role("user"),
            "assistantMessages": role("assistant"),
            "toolCalls": tool_calls,
            "toolResults": role("toolResult"),
            "totalMessages": role("user") + role("assistant"),
            "tokens": {
                "input": input,
                "output": output,
                "cacheRead": 0,
                "cacheWrite": 0,
                "total": input + output,
            },
            "cost": cost,
        })
    }

    /// prime-agent's `get_commands`: extension commands (ha has none), then
    /// prompt templates, then skills.
    fn commands(&self) -> Value {
        let mut prompts = Vec::new();
        let mut skills = Vec::new();
        for command in self.service.menu_commands() {
            let name = command.name.trim_start_matches('/').to_owned();
            let mut entry = json!({ "name": name, "source": command.tag });
            if !command.description.is_empty() {
                entry["description"] = json!(command.description);
            }
            if !command.argument_hint.is_empty() {
                entry["argumentHint"] = json!(command.argument_hint);
            }
            if command.tag == "skill" {
                skills.push(entry);
            } else {
                prompts.push(entry);
            }
        }
        prompts.extend(skills);
        json!({ "commands": prompts })
    }

    fn finish(&mut self, outcome: &RunOutcome) {
        self.running = false;
        if let Some(id) = self.refining.take() {
            let refined = self.refined.take();
            let response = match (outcome, self.last_error.take()) {
                (RunOutcome::Done, _) => success(id.as_ref(), "refine", refined),
                (_, message) => error(
                    id.as_ref(),
                    "refine",
                    message.as_deref().unwrap_or("refinement failed"),
                ),
            };
            self.emit(response);
        } else if let Some(id) = self.compacting.take() {
            let response = match (outcome, self.last_error.take()) {
                (RunOutcome::Done, _) => {
                    self.emit(
                        json!({ "type": "compaction_end", "reason": "manual", "aborted": false }),
                    );
                    success(id.as_ref(), "compact", None)
                }
                (_, error_message) => {
                    self.emit(
                        json!({ "type": "compaction_end", "reason": "manual", "aborted": true }),
                    );
                    error(
                        id.as_ref(),
                        "compact",
                        error_message.as_deref().unwrap_or("compaction failed"),
                    )
                }
            };
            self.emit(response);
        } else {
            let (stop_reason, error_message) = match outcome {
                RunOutcome::Canceled => ("aborted", None),
                RunOutcome::Failed(reason) => (
                    "error",
                    Some(self.last_error.take().unwrap_or_else(|| reason.clone())),
                ),
                _ => ("stop", None),
            };
            // A turn that failed before it streamed still ends on an
            // assistant message carrying the error, opened first.
            if error_message.is_some() && !self.message.started {
                self.message.started = true;
                let message = self.message.value(None, None);
                self.emit(json!({ "type": "message_start", "message": message }));
            }
            self.end_message(stop_reason, error_message.as_deref());
            let messages = std::mem::take(&mut self.messages);
            self.emit(json!({ "type": "agent_end", "messages": messages }));
        }
        if let Some((id, guidance)) = self.compact_next.take() {
            self.start_compaction(id, guidance);
            return;
        }
        if self.paused {
            return;
        }
        // prime-agent's queue pump: the steering lane first, then follow-ups,
        // each drained one at a time or all at once.
        let take = |lane: &mut VecDeque<String>, mode| match mode {
            super::queue::QueueMode::OneAtATime => lane.pop_front(),
            super::queue::QueueMode::All => {
                (!lane.is_empty()).then(|| lane.drain(..).collect::<Vec<_>>().join("\n\n"))
            }
        };
        let steering_mode = self.steering_mode;
        let follow_up_mode = self.follow_up_mode;
        if let Some(next) = take(&mut self.steering, steering_mode)
            .or_else(|| take(&mut self.follow_ups, follow_up_mode))
        {
            self.start(next);
        }
    }

    #[allow(clippy::too_many_lines)] // prime-agent's command table, one arm per command
    fn command(&mut self, id: Option<Value>, name: &str, payload: &Value) {
        let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_owned);
        let outcome: Result<Option<Value>, String> = match name {
            "prompt" => match text("message") {
                None => Err("prompt requires a message".to_owned()),
                Some(message) if self.running => {
                    match payload.get("streamingBehavior").and_then(Value::as_str) {
                        Some("steer") => {
                            self.steer(message);
                            Ok(None)
                        }
                        Some("followUp") => {
                            self.paused = false;
                            self.follow_ups.push_back(message);
                            Ok(None)
                        }
                        _ => Err(BUSY.to_owned()),
                    }
                }
                Some(message) => {
                    // The response goes before the turn's events.
                    self.emit(success(id.as_ref(), name, None));
                    self.start(message);
                    return;
                }
            },
            "steer" | "follow_up" => match text("message") {
                None => Err(format!("{name} requires a message")),
                Some(message) if self.running && name == "steer" => {
                    self.steer(message);
                    Ok(None)
                }
                Some(message) if self.running => {
                    self.paused = false;
                    self.follow_ups.push_back(message);
                    Ok(None)
                }
                Some(message) => {
                    self.emit(success(id.as_ref(), name, None));
                    self.start(message);
                    return;
                }
            },
            "abort" => {
                self.paused = true;
                self.service.cancel();
                Ok(None)
            }
            "new_session" => self
                .service
                .resume(None)
                .map(|()| Some(json!({ "cancelled": false }))),
            "get_state" => Ok(Some(self.state())),
            "set_model" => match (text("provider"), text("modelId")) {
                (None, _) => Err("set_model requires a provider".to_owned()),
                (_, None) => Err("set_model requires a modelId".to_owned()),
                (Some(provider), Some(model)) => self
                    .service
                    .set_model(&format!("{provider}/{model}"))
                    .map(|_| Some(json!({ "provider": provider, "id": model }))),
            },
            "cycle_model" => self.service.cycle_model(true).map(|_| {
                Some(json!({
                    "model": self.model(),
                    "thinkingLevel": self.service.thinking_level().unwrap_or_else(|| "off".to_owned()),
                    "isScoped": false,
                }))
            }),
            "get_available_models" => {
                let models = self
                    .service
                    .model_options()
                    .into_iter()
                    .map(|(reference, model_name)| {
                        let (provider, model) =
                            reference.split_once('/').unwrap_or(("", &reference));
                        json!({ "provider": provider, "id": model, "name": model_name })
                    })
                    .collect::<Vec<_>>();
                Ok(Some(json!({ "models": models })))
            }
            "set_thinking_level" => match text("level") {
                None => Err("Invalid thinking level: expected a string".to_owned()),
                Some(level) if !THINKING_LEVELS.contains(&level.as_str()) => Err(format!(
                    "Invalid thinking level \"{level}\". Valid values: {}",
                    THINKING_LEVELS.join(", ")
                )),
                Some(level) => self.service.set_thinking(&level).map(|_| None),
            },
            "compact" => {
                let guidance = text("customInstructions");
                if self.running {
                    // prime-agent's compaction aborts the running turn first.
                    self.service.cancel();
                    self.compact_next = Some((id, guidance));
                } else {
                    self.start_compaction(id, guidance);
                }
                return;
            }
            "cycle_thinking_level" => {
                let levels = self.service.thinking_levels();
                if levels.is_empty() {
                    Ok(Some(Value::Null))
                } else {
                    let current = self.service.thinking_level();
                    let next = match levels
                        .iter()
                        .position(|level| Some(level) == current.as_ref())
                    {
                        Some(index) => levels[(index + 1) % levels.len()].clone(),
                        None => levels[0].clone(),
                    };
                    self.service
                        .set_thinking(&next)
                        .map(|_| Some(json!({ "level": next })))
                }
            }
            "set_steering_mode" | "set_follow_up_mode" => match text("mode") {
                None => Err(format!("{name} requires a mode")),
                Some(mode) => match super::queue::QueueMode::parse(&mode) {
                    None => Err(format!(
                        "Invalid queue mode \"{mode}\". Valid values: all, one-at-a-time"
                    )),
                    Some(mode) => {
                        let steering = name == "set_steering_mode";
                        if steering {
                            self.steering_mode = mode;
                        } else {
                            self.follow_up_mode = mode;
                        }
                        self.service.set_queue_mode(steering, mode).map(|()| None)
                    }
                },
            },
            "refine" => {
                if self.running {
                    Err(BUSY.to_owned())
                } else {
                    self.running = true;
                    self.refining = Some(id);
                    self.refined = None;
                    self.last_error = None;
                    self.service.submit(SubmitRequest {
                        input_id: InputId::generate(),
                        text: "/refine".to_owned(),
                        answer_question_id: None,
                        shell_prefix: None,
                        compact_guidance: None,
                        refine: Some(super::refine::RefineOptions {
                            instructions: text("instructions"),
                            global: payload
                                .get("global")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            rollback: text("rollbackId"),
                            curate: false,
                        }),
                    });
                    return;
                }
            }
            "set_auto_compaction" | "set_auto_retry" => {
                match payload.get("enabled").and_then(Value::as_bool) {
                    None => Err(format!("{name} requires enabled")),
                    Some(enabled) if name == "set_auto_compaction" => {
                        self.service.set_auto_compaction(enabled).map(|()| None)
                    }
                    Some(enabled) => self.service.set_auto_retry(enabled).map(|()| None),
                }
            }
            // prime-agent's `abortRetry` always answers success.
            "abort_retry" => Ok(None),
            "switch_session" => match text("sessionPath") {
                None => Err("switch_session requires a sessionPath".to_owned()),
                Some(session) => self
                    .service
                    .resume(Some(session))
                    .map(|()| Some(json!({ "cancelled": false }))),
            },
            "fork" => match text("entryId") {
                None => Err("fork requires an entryId".to_owned()),
                Some(entry) => match self.service.list_turns(super::events::TurnsPurpose::Fork) {
                    Ok(()) => {
                        self.pending.push(Pending::Fork(id, entry));
                        return;
                    }
                    Err(message) => Err(message),
                },
            },
            "clone" => self
                .service
                .clone_conversation()
                .map(|()| Some(json!({ "cancelled": false })))
                .map_err(|_| "Cannot clone session: no current entry selected".to_owned()),
            "get_fork_messages" => {
                match self.service.list_turns(super::events::TurnsPurpose::Fork) {
                    Ok(()) => {
                        self.pending.push(Pending::ForkMessages(id));
                        return;
                    }
                    Err(message) => Err(message),
                }
            }
            "get_messages" | "get_session_stats" => match self.service.read_conversation() {
                Ok(()) => {
                    self.pending.push(if name == "get_messages" {
                        Pending::Messages(id)
                    } else {
                        Pending::Stats(id)
                    });
                    return;
                }
                Err(message) => Err(message),
            },
            "export_html" => match self.service.export_html(text("outputPath")) {
                Ok(()) => {
                    self.pending.push(Pending::Export(id));
                    return;
                }
                Err(message) => Err(message),
            },
            "get_commands" => Ok(Some(self.commands())),
            // prime-agent's in-process transport: no scheduler, no family,
            // no bash executor live here.
            "list_schedules" => Ok(Some(json!({ "jobs": [] }))),
            "list_heartbeats" => Ok(Some(json!({ "heartbeats": [] }))),
            "get_heartbeat" => Ok(Some(json!({ "heartbeat": Value::Null }))),
            "add_schedule" | "cancel_schedule" => Err(CRON_REQUIRES_DAEMON.to_owned()),
            "set_heartbeat" | "update_heartbeat" | "manage_heartbeat" => {
                Err(HEARTBEATS_REQUIRE_DAEMON.to_owned())
            }
            "send_message"
            | "agent_messages_status"
            | "agent_messages_pause"
            | "agent_messages_resume"
            | "agent_messages_clear" => Err(AGENT_MESSAGING_REQUIRES_DAEMON.to_owned()),
            "observe" => Err(format!(
                "Unknown active session: {}",
                text("activeSessionId").unwrap_or_default()
            )),
            "unobserve" | "abort_bash" => Ok(None),
            "bash" => Err(BASH_BACKEND_GAP.to_owned()),
            "get_last_assistant_text" => Ok(Some(json!({ "text": self.last_answer }))),
            "set_session_name" => match text("name").map(|name| name.trim().to_owned()) {
                None => Err("set_session_name requires a name".to_owned()),
                Some(name) if name.is_empty() => Err("Session name cannot be empty".to_owned()),
                Some(name) => self.service.rename(&name).map(|_| None),
            },
            unknown => Err(format!("Unknown command: {unknown}")),
        };
        let response = match outcome {
            Ok(data) => success(id.as_ref(), name, data),
            Err(message) => error(id.as_ref(), name, &message),
        };
        self.emit(response);
    }

    /// A steer into the running turn; one it cannot take yet waits in the
    /// steering lane.
    fn steer(&mut self, message: String) {
        self.paused = false;
        if self.service.steer(&message).is_err() {
            self.steering.push_back(message);
        }
    }

    fn model(&self) -> Value {
        self.service.current_model().map_or(
            Value::Null,
            |(provider, id)| json!({ "provider": provider, "id": id }),
        )
    }

    /// prime-agent's `RpcSessionState`, as far as ha knows it.
    fn state(&self) -> Value {
        let (steering_mode, follow_up_mode) = (self.steering_mode, self.follow_up_mode);
        let mode = |mode| match mode {
            super::queue::QueueMode::All => "all",
            super::queue::QueueMode::OneAtATime => "one-at-a-time",
        };
        let mut actions = json!({
            "queuedCount": self.steering.len() + self.follow_ups.len(),
            "steering": self.steering,
            "followUps": self.follow_ups,
        });
        if self.running {
            actions["active"] = json!({ "kind": "turn", "phase": "running" });
        }
        let mut state = json!({
            "thinkingLevel": self.service.thinking_level().unwrap_or_else(|| "off".to_owned()),
            "isStreaming": self.running && self.compacting.is_none(),
            "isCompacting": self.compacting.is_some(),
            "steeringMode": mode(steering_mode),
            "followUpMode": mode(follow_up_mode),
            "autoCompactionEnabled": self.service.auto_compaction(),
            "sessionActions": actions,
        });
        let model = self.model();
        if !model.is_null() {
            state["model"] = model;
        }
        if let Some(id) = self.service.conversation_id() {
            state["sessionId"] = json!(id);
        }
        state
    }
}

/// Serve the RPC mode until stdin closes.
///
/// # Errors
/// The launch could not be resolved.
pub async fn run(overrides: ConfigOverrides) -> Result<ExitCode, HarnessError> {
    let environment = super::paths::LaunchEnvironment::capture();
    let caller_dir = std::env::current_dir().map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("the current working directory could not be resolved: {error}"),
        )
    })?;
    let context = super::bootstrap::resolve(super::bootstrap::LaunchRequest {
        cwd: None,
        caller_dir,
        platform: super::paths::HostPlatform::current(),
        environment: environment.clone(),
        explicit_data_dir: None,
    })?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let service = super::service::AgentSessionService::new_with_overrides(
        &context,
        environment,
        sender,
        overrides,
    );
    let (lines_in, mut lines) = mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines_in.send(line).is_err() {
                break;
            }
        }
    });
    let (steering_mode, follow_up_mode) = service.queue_modes();
    let mut rpc = Rpc {
        service: Box::new(service),
        running: false,
        paused: false,
        steering: VecDeque::new(),
        follow_ups: VecDeque::new(),
        message: Message::default(),
        messages: Vec::new(),
        tools: Vec::new(),
        tool_output: None,
        last_answer: None,
        last_error: None,
        compacting: None,
        compact_next: None,
        refining: None,
        refined: None,
        pending: Vec::new(),
        steering_mode,
        follow_up_mode,
        out: Vec::new(),
    };
    let mut stdin_open = true;
    loop {
        tokio::select! {
            line = lines.recv(), if stdin_open => match line {
                Some(line) if line.trim().is_empty() => {}
                Some(line) => match parse_line(&line) {
                    Ok((id, name, payload)) => rpc.command(id, &name, &payload),
                    Err(response) => rpc.emit(response),
                },
                None => stdin_open = false,
            },
            Some(event) = events.recv() => rpc.event(event),
        }
        for frame in rpc.out.drain(..) {
            if let Ok(line) = serde_json::to_string(&frame) {
                println!("{line}");
            }
        }
        // stdin closed: the running turn settles, then the mode exits.
        if !stdin_open && !rpc.running {
            return Ok(ExitCode::SUCCESS);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{error, parse_line, success};
    use serde_json::json;

    #[test]
    fn frames_have_prime_agents_shape() {
        assert_eq!(
            success(Some(&json!("q1")), "get_state", None),
            json!({ "id": "q1", "type": "response", "command": "get_state", "success": true })
        );
        assert_eq!(
            error(None, "prompt", "nope"),
            json!({ "type": "response", "command": "prompt", "success": false, "error": "nope" })
        );
        let (id, name, _) =
            parse_line(r#"{"id": 7, "type": "prompt", "message": "hi"}"#).expect("a command");
        assert_eq!((id, name.as_str()), (Some(json!(7)), "prompt"));
        assert_eq!(
            parse_line("[1]").unwrap_err()["error"],
            json!("Invalid command: expected an object with a string type")
        );
        assert!(
            parse_line("{oops").unwrap_err()["error"]
                .as_str()
                .is_some_and(|text| text.starts_with("Failed to parse command:"))
        );
    }
}
