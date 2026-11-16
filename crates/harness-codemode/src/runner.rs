//! Running a script: [`run`] on the calling thread with a host that answers
//! calls synchronously, and [`execute`], which runs it on a thread of its
//! own and answers its calls from async code.
//!
//! [`execute`] is pi's worker-thread host without the worker: the script
//! computes on its own thread, so a spinning loop never blocks the async
//! runtime; its calls come back as messages; the caller owns the deadline
//! and the cancellation. When either fires, the script is told to stop at
//! its next poll, and a call it waits on is answered with the stop. A
//! script stuck in one long built-in is not waited for past a grace period:
//! the result says it timed out, and the thread ends on its own.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use indexmap::IndexMap;

use crate::interp::{Fatal, Host, HostCall, Interp, Limits, Outcome};
use crate::value::{Value, json_stringify};
use crate::{
    CallRecord, CallStatus, CodemodeResult, ErrorKind, ScriptError, StoreWrites, ToolInfo,
};

/// How long a stopped script may take to notice before its result is given
/// up on.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// The interpreter's thread: deep recursion in a script is Rust recursion.
const STACK_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RunOptions {
    /// The whole run, tool calls included; `None` for no deadline.
    pub timeout: Option<Duration>,
    pub memory_bytes: usize,
    pub max_output_chars: usize,
    pub max_output_items: usize,
    pub max_depth: usize,
}

impl Default for RunOptions {
    /// pi's defaults: five minutes, 16 Mi characters and 100,000 items of
    /// output; memory and depth for a coding agent's glue code.
    fn default() -> Self {
        Self {
            timeout: Some(Duration::from_mins(5)),
            memory_bytes: 512 * 1024 * 1024,
            max_output_chars: 16 * 1024 * 1024,
            max_output_items: 100_000,
            max_depth: 400,
        }
    }
}

fn error_of(value: &Value) -> ScriptError {
    let name = value.own("name").map(|name| crate::value::to_string(&name));
    let message = value.own("message").map_or_else(
        || crate::builtins::format_value(value),
        |message| crate::value::to_string(&message),
    );
    let stack = if value.is_error() {
        value
            .own("stack")
            .map(|stack| crate::value::to_string(&stack))
    } else {
        None
    };
    ScriptError {
        kind: ErrorKind::Script,
        name: if value.is_error() { name } else { None },
        message,
        stack,
    }
}

fn fatal_error(fatal: Fatal) -> ScriptError {
    let (kind, name, message) = match fatal {
        Fatal::Timeout(message) => (ErrorKind::Timeout, None, message),
        Fatal::Aborted(message) => (ErrorKind::Aborted, None, message),
        Fatal::Sandbox(message) => (ErrorKind::Sandbox, None, message),
        Fatal::Script(name, message) => (ErrorKind::Script, Some(name), message),
    };
    let stack = name.as_ref().map(|name| format!("{name}: {message}"));
    ScriptError {
        kind,
        name,
        message,
        stack,
    }
}

/// Run `code` (an async function body) and wait for it, on a thread with
/// the stack a deeply nested or recursive script needs.
pub fn run(
    code: &str,
    tools: Vec<ToolInfo>,
    store: Vec<(String, String)>,
    options: &RunOptions,
    cancel: Arc<AtomicBool>,
    host: &mut (dyn Host + Send),
) -> CodemodeResult {
    std::thread::scope(|scope| {
        let spawned = std::thread::Builder::new()
            .name("ha-codemode".to_owned())
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, move || {
                run_here(code, tools, store, options, cancel, host)
            });
        match spawned {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|_| stopped(ErrorKind::Sandbox, "the script's thread panicked")),
            Err(error) => stopped(
                ErrorKind::Sandbox,
                &format!("could not start the script's thread: {error}"),
            ),
        }
    })
}

/// [`run`] on the calling thread, whose stack must be [`STACK_BYTES`].
fn run_here(
    code: &str,
    tools: Vec<ToolInfo>,
    store: Vec<(String, String)>,
    options: &RunOptions,
    cancel: Arc<AtomicBool>,
    host: &mut dyn Host,
) -> CodemodeResult {
    let started = Instant::now();
    let body = match crate::parser::parse_script(code) {
        Ok(body) => body,
        Err(error) => {
            return CodemodeResult {
                outcome: Err(ScriptError {
                    kind: ErrorKind::Script,
                    name: Some("SyntaxError".to_owned()),
                    stack: Some(format!(
                        "SyntaxError: {}\n    at codemode.js:{}:{}",
                        error.message, error.line, error.col
                    )),
                    message: error.message,
                }),
                output: Vec::new(),
                calls: Vec::new(),
                store_writes: StoreWrites::default(),
            };
        }
    };
    let limits = Limits {
        deadline: options.timeout.map(|timeout| started + timeout),
        cancel,
        max_output_chars: options.max_output_chars,
        max_output_items: options.max_output_items,
        memory_bytes: options.memory_bytes,
        max_depth: options.max_depth,
    };
    let store: IndexMap<String, String> = store.into_iter().collect();
    let mut interp = Interp::new(host, limits, tools, store);
    let outcome = interp.run(&body);
    let outcome = match outcome {
        Outcome::Returned(value) => match json_stringify(&value, "") {
            Ok(json) => Ok(json),
            Err(crate::value::JsonError::Cycle) => Err(ScriptError {
                kind: ErrorKind::Script,
                name: Some("TypeError".to_owned()),
                message: "Converting circular structure to JSON: the script's return value"
                    .to_owned(),
                stack: None,
            }),
            Err(crate::value::JsonError::Unsupported(what)) => Err(ScriptError {
                kind: ErrorKind::Script,
                name: Some("TypeError".to_owned()),
                message: format!("the script returned {what}, which JSON cannot hold"),
                stack: None,
            }),
        },
        Outcome::Exited => Ok(None),
        Outcome::Threw(value) => Err(error_of(&value)),
        Outcome::Fatal(fatal) => Err(fatal_error(fatal)),
    };
    let store_writes = if outcome.is_ok() {
        let mut writes = StoreWrites::default();
        for (key, json) in std::mem::take(&mut interp.writes) {
            match json {
                Some(json) => writes.set.push((key, json)),
                None => writes.delete.push(key),
            }
        }
        writes
    } else {
        StoreWrites::default()
    };
    CodemodeResult {
        outcome,
        output: std::mem::take(&mut interp.output),
        calls: std::mem::take(&mut interp.calls),
        store_writes,
    }
}

type Reply = std::sync::mpsc::SyncSender<Result<Vec<Result<String, String>>, Fatal>>;

/// A host on the script's thread that hands each batch of calls to the
/// async side and waits for the answers.
struct ChannelHost {
    requests: tokio::sync::mpsc::UnboundedSender<(Vec<HostCall>, Reply)>,
}

impl Host for ChannelHost {
    fn call(&mut self, calls: Vec<HostCall>) -> Result<Vec<Result<String, String>>, Fatal> {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        if self.requests.send((calls, reply)).is_err() {
            return Err(Fatal::Aborted("Execution aborted".to_owned()));
        }
        answer
            .recv()
            .unwrap_or_else(|_| Err(Fatal::Aborted("Execution aborted".to_owned())))
    }
}

fn stopped(kind: ErrorKind, message: &str) -> CodemodeResult {
    CodemodeResult {
        outcome: Err(ScriptError {
            kind,
            name: None,
            message: message.to_owned(),
            stack: None,
        }),
        output: Vec::new(),
        calls: Vec::new(),
        store_writes: StoreWrites::default(),
    }
}

const ABORTED: (ErrorKind, &str) = (ErrorKind::Aborted, "Execution aborted");
const TIMED_OUT: (ErrorKind, &str) = (ErrorKind::Timeout, "Execution timed out");

/// Run `code` on a thread of its own. `call` runs one batch of the script's
/// calls and answers each (the JSON text of its value, or its error
/// message); `cancelled` resolves when the caller gives up on the run.
pub async fn execute<F, Fut, C>(
    code: String,
    tools: Vec<ToolInfo>,
    store: Vec<(String, String)>,
    options: RunOptions,
    mut call: F,
    cancelled: C,
) -> CodemodeResult
where
    F: FnMut(Vec<HostCall>) -> Fut,
    Fut: Future<Output = Vec<Result<String, String>>>,
    C: Future<Output = ()>,
{
    let cancel = Arc::new(AtomicBool::new(false));
    let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let (done, mut finished) = tokio::sync::oneshot::channel();
    let deadline = options
        .timeout
        .map(|timeout| tokio::time::Instant::now() + timeout + Duration::from_millis(50));
    let thread_cancel = Arc::clone(&cancel);
    let spawned = std::thread::Builder::new()
        .name("ha-codemode".to_owned())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            let mut host = ChannelHost { requests };
            let result = run_here(&code, tools, store, &options, thread_cancel, &mut host);
            let _ = done.send(result);
        });
    if let Err(error) = spawned {
        return stopped(
            ErrorKind::Sandbox,
            &format!("could not start the script's thread: {error}"),
        );
    }
    // No deadline is a deadline nobody reaches.
    let deadline =
        deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_hours(876_000));
    let sleep = tokio::time::sleep_until(deadline);
    tokio::pin!(sleep);
    tokio::pin!(cancelled);
    // Each of `cancelled` and `sleep` is polled until it fires, never after.
    let stop = loop {
        tokio::select! {
            biased;
            result = &mut finished => {
                return result.unwrap_or_else(|_| stopped(ErrorKind::Sandbox, "the script's thread stopped before it reported a result"));
            }
            () = &mut cancelled => break ABORTED,
            () = &mut sleep => break TIMED_OUT,
            request = incoming.recv() => {
                let Some((calls, reply)) = request else { continue };
                let (answers, stop) = tokio::select! {
                    answers = call(calls) => (Ok(answers), None),
                    () = &mut cancelled => (Err(Fatal::Aborted(ABORTED.1.to_owned())), Some(ABORTED)),
                    () = &mut sleep => (Err(Fatal::Timeout(TIMED_OUT.1.to_owned())), Some(TIMED_OUT)),
                };
                if stop.is_some() {
                    cancel.store(true, Ordering::Relaxed);
                }
                let _ = reply.send(answers);
                if let Some(stop) = stop {
                    break stop;
                }
            }
        }
    };
    cancel.store(true, Ordering::Relaxed);
    // The script stops at its next poll; a call it waits on is answered
    // with the stop.
    let grace = tokio::time::sleep(STOP_GRACE);
    tokio::pin!(grace);
    loop {
        tokio::select! {
            result = &mut finished => {
                let mut result = result.unwrap_or_else(|_| stopped(stop.0, stop.1));
                if let Err(error) = &mut result.outcome
                    && error.kind != stop.0
                {
                    *error = ScriptError { kind: stop.0, name: None, message: stop.1.to_owned(), stack: None };
                }
                if result.outcome.is_ok() {
                    result.outcome = Err(ScriptError { kind: stop.0, name: None, message: stop.1.to_owned(), stack: None });
                }
                result.store_writes = StoreWrites::default();
                return result;
            }
            request = incoming.recv() => {
                if let Some((_, reply)) = request {
                    let fatal = if stop.0 == ErrorKind::Timeout {
                        Fatal::Timeout(stop.1.to_owned())
                    } else {
                        Fatal::Aborted(stop.1.to_owned())
                    };
                    let _ = reply.send(Err(fatal));
                }
            }
            () = &mut grace => return stopped(stop.0, &format!("{}; the script did not stop within {} s, and its output is lost", stop.1, STOP_GRACE.as_secs())),
        }
    }
}

fn cut_middle(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars.saturating_sub(head);
    let first: String = text.chars().take(head).collect();
    let last: String = text.chars().skip(count - tail).collect();
    format!(
        "{first}\n[... {} characters of output cut; print a summary instead, or set a larger max_output_tokens in the // @options line ...]\n{last}",
        count - head - tail
    )
}

/// The text the model reads for a run: the output in order, then what the
/// script returned (`=> value`) or the error, and the calls that did not
/// succeed. At most `max_chars` characters of output, cut in the middle.
#[must_use]
pub fn render_result(result: &CodemodeResult, max_chars: usize) -> String {
    let mut text = String::new();
    for item in &result.output {
        text.push_str(&item.text);
        text.push('\n');
    }
    let mut text = cut_middle(text.trim_end_matches('\n'), max_chars);
    let mut push_line = |line: &str| {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    };
    match &result.outcome {
        Ok(Some(value)) => push_line(&format!("=> {}", cut_middle(value, max_chars))),
        Ok(None) => {}
        Err(error) => {
            let detail = error.stack.clone().unwrap_or_else(|| match &error.name {
                Some(name) => format!("{name}: {}", error.message),
                None => error.message.clone(),
            });
            let label = match error.kind {
                ErrorKind::Script => "Script error",
                ErrorKind::Timeout => "Timed out",
                ErrorKind::Aborted => "Aborted",
                ErrorKind::Sandbox => "Codemode failed",
            };
            push_line(&format!("{label}: {detail}"));
        }
    }
    let failed: Vec<&CallRecord> = result
        .calls
        .iter()
        .filter(|call| call.status != CallStatus::Ok)
        .collect();
    if !failed.is_empty() {
        let shown: Vec<String> = failed
            .iter()
            .take(20)
            .map(|call| match call.status {
                CallStatus::Cancelled => {
                    format!("{} (never awaited, so it did not run)", call.name)
                }
                _ => format!("{} failed", call.name),
            })
            .collect();
        let more = failed.len().saturating_sub(20);
        push_line(&format!(
            "[{} of {} tool calls did not succeed: {}{}]",
            failed.len(),
            result.calls.len(),
            shown.join(", "),
            if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            }
        ));
    }
    if text.is_empty() {
        text.push_str("(the script printed nothing and returned undefined)");
    }
    text
}
