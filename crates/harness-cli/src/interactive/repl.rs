//! The `ipython` tool: prime-agent's persistent Python REPL.
//!
//! prime-agent runs its agent through one long-lived Python kernel: the model writes
//! cells, variables persist across cells and turns, `bash()` starts commands in the
//! background and returns a handle, and `rlm.*` calls reach back into the host. This
//! is the same kernel: the runtime is prime-agent's own `rlm` package, vendored under
//! `crates/harness-cli/python` and written to the data directory on first use, and
//! this module is the host side of its JSON-lines protocol (prime-agent's
//! `kernel/repl-manager.ts`, protocol described in its `repl.md`):
//!
//! - the kernel starts on the first `ipython` call and lives for the whole app
//!   session, so state carries from turn to turn; a kernel that dies is restarted
//!   on the next call, and the result says the old state is gone;
//! - one cell runs at a time; a cell that outlives its time is interrupted, and a
//!   cell left running by a canceled turn is interrupted before the next one runs;
//! - every `host_request` the runtime sends is answered - by the turn's handler when
//!   it knows the request, with an error otherwise - because an unanswered request
//!   would leave the cell waiting forever.
//!
//! Running Python is running code, so the tool passes the same approval as
//! `run_shell`. It is offered only when a Python 3.11+ interpreter is found
//! (`HA_PYTHON`, else `python3`/`python`/`py -3` on `PATH`); `HA_REPL=off` removes it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, OnceCell, mpsc};
use tokio::time::Instant;

use super::paths::LaunchEnvironment;

/// `off` (or `0`, `false`, `no`) removes the REPL.
pub const REPL_VARIABLE: &str = "HA_REPL";
/// An explicit interpreter for the kernel.
pub const PYTHON_VARIABLE: &str = "HA_PYTHON";
/// An explicit POSIX shell for `bash()`; on Windows Git Bash is found by itself.
pub const SHELL_VARIABLE: &str = "HA_REPL_SHELL";

/// The protocol the vendored runtime speaks.
const PROTOCOL_VERSION: u64 = 3;
/// How long one cell may run by default before it is interrupted.
pub const CELL_TIMEOUT_MS: u64 = 10 * 60 * 1000;
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long an interrupted cell gets to report `done` before the kernel is restarted.
const INTERRUPT_GRACE_MS: u64 = 10_000;
const INTERRUPT_GRACE: Duration = Duration::from_millis(INTERRUPT_GRACE_MS);
/// The most text one cell returns; the rest is cut with a note.
const MAX_CELL_OUTPUT_CHARS: usize = 200_000;
/// What the model is told when the state it had is gone.
const KERNEL_RESTART_NOTICE: &str =
    "[The Python kernel was restarted; variables and imports from earlier cells are gone.]";

/// The vendored runtime, embedded so a single binary carries it.
const RUNTIME_FILES: [(&str, &str); 5] = [
    ("__init__.py", include_str!("../../python/rlm/__init__.py")),
    ("repl.py", include_str!("../../python/rlm/repl.py")),
    ("bash.py", include_str!("../../python/rlm/bash.py")),
    ("_winjob.py", include_str!("../../python/rlm/_winjob.py")),
    ("harness.py", include_str!("../../python/rlm/harness.py")),
];

/// Code every new kernel runs first, as prime-agent's bootstrap cell does: the
/// `rlm` namespace and `bash` are globals, and output has no colour codes.
const BOOTSTRAP_CODE: &str = "import asyncio\n\
import os as _ha_os\n\
_ha_os.environ[\"NO_COLOR\"] = \"1\"\n\
import rlm as _ha_rlm_module\n\
rlm = _ha_rlm_module.rlm\n\
bash = _ha_rlm_module.bash\n\
del _ha_os";

/// The answer to one host request: `None` when the type has no handler.
pub type HostReply<'a> = Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + Send + 'a>>;

/// Answers the `host_request`s one turn knows.
pub trait HostRequests: Send + Sync {
    /// Answer one request, or `None` when this turn has no handler for its type.
    fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a>;
}

/// No host request is known: every one is answered with an error.
pub struct NoHostRequests;

impl HostRequests for NoHostRequests {
    fn handle<'a>(&'a self, _request: &'a Value) -> HostReply<'a> {
        Box::pin(async { None })
    }
}

/// The REPL of one app session: found once, started on first use, kept across turns.
pub struct ReplShared {
    data_dir: PathBuf,
    workspace: PathBuf,
    python_override: Option<String>,
    shell: Option<PathBuf>,
    python: OnceCell<Option<PathBuf>>,
    kernel: Mutex<KernelSlot>,
}

#[derive(Default)]
struct KernelSlot {
    kernel: Option<Kernel>,
    /// A kernel ran before and is gone, so the next result says so.
    lost: bool,
}

impl ReplShared {
    /// The REPL as the environment configures it, or `None` when turned off.
    #[must_use]
    pub fn from_environment(
        environment: &LaunchEnvironment,
        data_dir: &Path,
        workspace: &Path,
    ) -> Option<Arc<Self>> {
        let value = |name: &str| {
            environment
                .value(name)
                .and_then(|value| value.to_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        if value(REPL_VARIABLE).is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            )
        }) {
            return None;
        }
        let shell = value(SHELL_VARIABLE)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(default_shell);
        Some(Arc::new(Self {
            data_dir: data_dir.to_path_buf(),
            workspace: workspace.to_path_buf(),
            python_override: value(PYTHON_VARIABLE),
            shell,
            python: OnceCell::new(),
            kernel: Mutex::new(KernelSlot::default()),
        }))
    }

    /// The interpreter the kernel runs on, found once.
    pub async fn python(&self) -> Option<&PathBuf> {
        self.python
            .get_or_init(|| find_python(self.python_override.clone()))
            .await
            .as_ref()
    }

    /// Run one cell, starting the kernel when there is none.
    async fn execute(
        &self,
        code: &str,
        timeout: Duration,
        host: &dyn HostRequests,
    ) -> Result<String, HarnessError> {
        let python = self.python().await.cloned().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::PolicyDenied,
                "no Python 3.11+ interpreter was found for the REPL",
            )
        })?;
        let mut slot = self.kernel.lock().await;
        let mut notice = None;
        if slot.kernel.is_none() {
            if std::mem::take(&mut slot.lost) {
                notice = Some(KERNEL_RESTART_NOTICE);
            }
            let runtime = write_runtime(&self.data_dir)
                .map_err(|error| HarnessError::new(ErrorCode::StorageWriteFailed, error))?;
            let kernel = Kernel::start(
                &python,
                &runtime,
                &self.workspace,
                &self.data_dir,
                self.shell.as_deref(),
            )
            .await
            .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
            slot.kernel = Some(kernel);
        }
        let kernel = slot.kernel.as_mut().expect("the kernel was just started");
        let outcome = kernel.execute(code, timeout, host).await;
        let text = match outcome {
            Ok(cell) => cell,
            Err(dead) => {
                // The process is gone or stuck: drop it, and the next call starts over.
                slot.kernel = None;
                slot.lost = true;
                dead
            }
        };
        Ok(match notice {
            Some(notice) if text.is_empty() => notice.to_owned(),
            Some(notice) => format!("{notice}\n\n{text}"),
            None => text,
        })
    }
}

/// One running kernel process.
struct Kernel {
    _child: Child,
    stdin: ChildStdin,
    events: mpsc::UnboundedReceiver<Value>,
    next: u64,
    /// A cell that was started and whose `done` was never read.
    in_flight: Option<String>,
}

impl Kernel {
    async fn start(
        python: &Path,
        runtime: &Path,
        workspace: &Path,
        data_dir: &Path,
        shell: Option<&Path>,
    ) -> Result<Self, String> {
        let mut command = Command::new(python);
        command
            .args(["-u", "-m", "rlm.repl"])
            .current_dir(workspace)
            .env("PYTHONPATH", runtime)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .env(
                "PRIME_AGENT_KERNEL_OWNER_PID",
                std::process::id().to_string(),
            )
            .env("PRIME_AGENT_CODING_AGENT_DIR", data_dir.join("repl"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(shell) = shell {
            command.env("PRIME_AGENT_BASH_SHELL", shell);
        }
        #[cfg(windows)]
        {
            // No console window flashes up for the kernel.
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("the Python kernel could not start: {error}"))?;
        let stdin = child.stdin.take().ok_or("the kernel has no stdin")?;
        let stdout = child.stdout.take().ok_or("the kernel has no stdout")?;
        let (sender, events) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(event) = serde_json::from_str::<Value>(&line)
                    && sender.send(event).is_err()
                {
                    break;
                }
            }
        });
        let mut kernel = Self {
            _child: child,
            stdin,
            events,
            next: 0,
            in_flight: None,
        };
        let ready = tokio::time::timeout(READY_TIMEOUT, kernel.events.recv())
            .await
            .map_err(|_| "the Python kernel did not start in time".to_owned())?
            .ok_or("the Python kernel exited while starting")?;
        if ready.get("event").and_then(Value::as_str) != Some("ready")
            || ready.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION)
        {
            return Err(format!(
                "the Python kernel spoke an unknown protocol: {ready}"
            ));
        }
        let boot = kernel
            .execute(BOOTSTRAP_CODE, READY_TIMEOUT, &NoHostRequests)
            .await
            .map_err(|error| format!("the Python kernel failed to start: {error}"))?;
        if boot.contains("Traceback") {
            return Err(format!("the REPL runtime failed to load: {boot}"));
        }
        Ok(kernel)
    }

    async fn send(&mut self, request: &Value) -> Result<(), String> {
        let mut line = request.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("the Python kernel stopped: {error}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|error| format!("the Python kernel stopped: {error}"))
    }

    /// Run one cell. `Ok` carries the cell's text; `Err` means the kernel is unusable
    /// and carries what to tell the model.
    async fn execute(
        &mut self,
        code: &str,
        timeout: Duration,
        host: &dyn HostRequests,
    ) -> Result<String, String> {
        if let Some(stale) = self.in_flight.take() {
            // A canceled turn left a cell running; stop it before this one starts.
            self.send(&json!({"type": "interrupt", "id": stale}))
                .await?;
            let mut ignored = CellText::default();
            if !self
                .drain(&stale, Instant::now() + INTERRUPT_GRACE, host, &mut ignored)
                .await?
            {
                return Err(
                    "the previous Python cell could not be stopped; the kernel was restarted"
                        .to_owned(),
                );
            }
        }
        self.next += 1;
        let id = format!("cell-{}", self.next);
        self.send(&json!({"type": "execute", "id": id, "code": code}))
            .await?;
        self.in_flight = Some(id.clone());
        let mut text = CellText::default();
        if self
            .drain(&id, Instant::now() + timeout, host, &mut text)
            .await?
        {
            self.in_flight = None;
            return Ok(text.render());
        }
        // Out of time: interrupt, as Ctrl-C does in prime-agent, and give it a moment.
        self.send(&json!({"type": "interrupt", "id": id})).await?;
        text.note(&format!(
            "[The cell ran longer than {}s and was interrupted.]",
            timeout.as_secs()
        ));
        if self
            .drain(&id, Instant::now() + INTERRUPT_GRACE, host, &mut text)
            .await?
        {
            self.in_flight = None;
            return Ok(text.render());
        }
        text.note(&format!(
            "{KERNEL_RESTART_NOTICE} It did not stop after the interrupt."
        ));
        Err(text.render())
    }

    /// Read events until cell `id` reports `done` (true) or `deadline` passes (false).
    async fn drain(
        &mut self,
        id: &str,
        deadline: Instant,
        host: &dyn HostRequests,
        text: &mut CellText,
    ) -> Result<bool, String> {
        loop {
            let event = match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Err(_) => return Ok(false),
                Ok(None) => {
                    text.note("[The Python kernel exited.]");
                    return Err(text.render());
                }
                Ok(Some(event)) => event,
            };
            let kind = event.get("event").and_then(Value::as_str).unwrap_or("");
            let owner = event.get("id").and_then(Value::as_str);
            let ours = owner == Some(id);
            match kind {
                "stdout" | "stderr" => {
                    let chunk = event.get("text").and_then(Value::as_str).unwrap_or("");
                    match (ours, kind) {
                        (true, "stdout") => text.stdout.push_str(chunk),
                        (true, _) => text.stderr.push_str(chunk),
                        (false, _) if owner.is_none() => text.background.push_str(chunk),
                        _ => {}
                    }
                }
                "result" if ours => {
                    event
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .clone_into(&mut text.result);
                }
                "error" if ours => {
                    let traceback = event
                        .get("traceback")
                        .and_then(Value::as_array)
                        .map(|lines| lines.iter().filter_map(Value::as_str).collect::<String>())
                        .unwrap_or_default();
                    text.error = if traceback.is_empty() {
                        format!(
                            "{}: {}",
                            event
                                .get("ename")
                                .and_then(Value::as_str)
                                .unwrap_or("Error"),
                            event.get("evalue").and_then(Value::as_str).unwrap_or("")
                        )
                    } else {
                        traceback
                    };
                }
                "host_request" => {
                    let request_id = owner.unwrap_or_default().to_owned();
                    let data = event.get("data").cloned().unwrap_or(Value::Null);
                    let reply = match host.handle(&data).await {
                        Some(Ok(result)) => json!({"status": "ok", "result": result}),
                        Some(Err(error)) => json!({"status": "error", "error": error}),
                        None => json!({
                            "status": "error",
                            "error": format!(
                                "{} is not available in ha",
                                data.get("type").and_then(Value::as_str).unwrap_or("this host request")
                            ),
                        }),
                    };
                    self.send(&json!({"type": "host_reply", "id": request_id, "data": reply}))
                        .await?;
                }
                "done" if ours => return Ok(true),
                _ => {}
            }
        }
    }
}

/// The text one cell produced, in prime-agent's order.
#[derive(Default)]
struct CellText {
    stdout: String,
    stderr: String,
    result: String,
    error: String,
    background: String,
    notes: Vec<String>,
}

impl CellText {
    fn note(&mut self, note: &str) {
        self.notes.push(note.to_owned());
    }

    fn render(&self) -> String {
        let mut text = String::new();
        for part in [&self.stdout, &self.stderr, &self.result, &self.error] {
            if part.is_empty() {
                continue;
            }
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(part);
        }
        if !self.background.is_empty() {
            let _ = write!(
                text,
                "{}[background output (unattributed)]\n{}",
                if text.is_empty() { "" } else { "\n" },
                self.background
            );
        }
        for note in &self.notes {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(note);
        }
        if text.chars().count() > MAX_CELL_OUTPUT_CHARS {
            let kept = text.chars().take(MAX_CELL_OUTPUT_CHARS).collect::<String>();
            text = format!(
                "{kept}\n[output cut at {MAX_CELL_OUTPUT_CHARS} characters; keep large results in variables or files]"
            );
        }
        text
    }
}

/// Write the vendored runtime under the data directory, once per content version.
fn write_runtime(data_dir: &Path) -> Result<PathBuf, String> {
    // FNV-1a over the files: a new build with a changed runtime gets its own directory.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, body) in RUNTIME_FILES {
        for byte in name.bytes().chain(body.bytes()) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    let root = data_dir.join("runtime").join(format!("rlm-{hash:016x}"));
    let package = root.join("rlm");
    std::fs::create_dir_all(&package)
        .map_err(|error| format!("the REPL runtime could not be written: {error}"))?;
    for (name, body) in RUNTIME_FILES {
        let path = package.join(name);
        if std::fs::read_to_string(&path).is_ok_and(|current| current == body) {
            continue;
        }
        std::fs::write(&path, body)
            .map_err(|error| format!("the REPL runtime could not be written: {error}"))?;
    }
    Ok(root)
}

/// The first interpreter that is Python 3.11 or newer.
async fn find_python(explicit: Option<String>) -> Option<PathBuf> {
    let candidates: Vec<Vec<String>> = if let Some(path) = explicit {
        vec![vec![path]]
    } else {
        let mut list = vec![vec!["python3".to_owned()], vec!["python".to_owned()]];
        if cfg!(windows) {
            list.push(vec!["py".to_owned(), "-3".to_owned()]);
        }
        list
    };
    for candidate in candidates {
        let mut command = Command::new(&candidate[0]);
        command
            .args(&candidate[1..])
            .args([
                "-c",
                "import sys; print(sys.executable); print(sys.version_info >= (3, 11))",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000);
        }
        let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(10), command.output()).await
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines();
        let (Some(executable), Some("True")) = (lines.next(), lines.next().map(str::trim)) else {
            continue;
        };
        let executable = PathBuf::from(executable.trim());
        if executable.is_absolute() {
            return Some(executable);
        }
    }
    None
}

/// The POSIX shell `bash()` runs commands in.
///
/// prime-agent's runtime never looks up the shell on `PATH` on Windows (a repository
/// could plant one there), so the host passes an absolute path: Git Bash where it is
/// installed.
fn default_shell() -> Option<PathBuf> {
    let candidates: Vec<PathBuf> = if cfg!(windows) {
        let mut list = Vec::new();
        for variable in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(variable) {
                list.push(PathBuf::from(root).join("Git").join("bin").join("bash.exe"));
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            list.push(
                PathBuf::from(local)
                    .join("Programs")
                    .join("Git")
                    .join("bin")
                    .join("bash.exe"),
            );
        }
        list
    } else {
        vec![PathBuf::from("/bin/bash"), PathBuf::from("/bin/sh")]
    };
    candidates.into_iter().find(|path| path.is_file())
}

/// The `ipython` tool of one turn.
#[derive(Clone)]
pub struct ReplHost {
    shared: Arc<ReplShared>,
    host: Arc<dyn HostRequests>,
}

impl ReplHost {
    /// The tool for this turn, when an interpreter exists.
    pub async fn for_turn(shared: &Arc<ReplShared>, host: Arc<dyn HostRequests>) -> Option<Self> {
        shared.python().await?;
        Some(Self {
            shared: Arc::clone(shared),
            host,
        })
    }

    #[must_use]
    #[allow(
        clippy::unused_self,
        reason = "a method, like the other hosts' `tools`, so callers map every host the same way"
    )]
    pub fn tools(&self) -> ExternalTools {
        ExternalTools::new(Arc::new(ReplCatalog))
    }

    #[must_use]
    pub fn dispatcher(&self) -> Arc<dyn ExternalToolDispatcher> {
        Arc::new(self.clone())
    }

    fn code(arguments: &Value) -> Result<&str, HarnessError> {
        let object = arguments.as_object().ok_or_else(|| {
            HarnessError::new(ErrorCode::InvalidPayload, "ipython takes an object")
        })?;
        if object.keys().any(|key| key != "code") {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "ipython accepts only code",
            ));
        }
        object
            .get("code")
            .and_then(Value::as_str)
            .filter(|code| !code.trim().is_empty())
            .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "ipython needs code"))
    }
}

struct ReplCatalog;

impl ExternalToolCatalog for ReplCatalog {
    fn schemas(&self) -> Vec<Value> {
        vec![json!({
            "type": "function",
            "function": {
                "name": "ipython",
                "description": "Execute Python code in a persistent Python REPL. Top-level `await` is supported. Variables, imports, and loaded data persist across calls. Run shell commands with `bash('cmd')` / `await bash('cmd')`. Project imports, tests, scripts, CLIs, and dependency checks should run through the target project's own environment.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "code": {
                            "type": "string",
                            "description": "Python code to execute in the persistent Python REPL. Use the target project's own environment for project imports, tests, scripts, CLIs, and dependency checks instead of direct kernel imports."
                        }
                    },
                    "required": ["code"],
                    "additionalProperties": false
                }
            }
        })]
    }

    fn resolve(&self, name: &str, arguments: &Value) -> Option<CodingToolAction> {
        (name == "ipython").then(|| CodingToolAction::ExternalTool {
            plugin_id: "repl".to_owned(),
            tool_name: name.to_owned(),
            arguments: arguments.clone(),
            parent_invocation_id: None,
            timeout_ms: CELL_TIMEOUT_MS + 2 * INTERRUPT_GRACE_MS,
        })
    }
}

impl ExternalToolDispatcher for ReplHost {
    fn validate_external<'a>(
        &'a self,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if plugin_id != "repl" || tool_name != "ipython" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "REPL tool target is unavailable",
                ));
            }
            Self::code(arguments).map(|_| ())
        })
    }

    fn dispatch_external<'a>(
        &'a self,
        _authorization: &'a ToolDispatchAuthorization,
        plugin_id: &'a str,
        tool_name: &'a str,
        arguments: &'a Value,
        _timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, HarnessError>> + Send + 'a>> {
        Box::pin(async move {
            if plugin_id != "repl" || tool_name != "ipython" {
                return Err(HarnessError::new(
                    ErrorCode::PolicyDenied,
                    "REPL tool target is unavailable",
                ));
            }
            let code = Self::code(arguments)?;
            let text = self
                .shared
                .execute(
                    code,
                    Duration::from_millis(CELL_TIMEOUT_MS),
                    self.host.as_ref(),
                )
                .await?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "repl".to_owned(),
                tool_name: "ipython".to_owned(),
                payload: json!({ "text": if text.is_empty() { "(no output)".to_owned() } else { text } }),
                inflight: 1,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CellText, NoHostRequests, ReplShared, write_runtime};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn cell_text_keeps_prime_agents_order() {
        let text = CellText {
            stdout: "out\n".to_owned(),
            stderr: "warn".to_owned(),
            result: "42".to_owned(),
            error: String::new(),
            background: "late".to_owned(),
            notes: vec!["[note]".to_owned()],
        };
        assert_eq!(
            text.render(),
            "out\nwarn\n42\n[background output (unattributed)]\nlate\n[note]"
        );
    }

    #[test]
    fn the_runtime_is_written_once_per_version() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first = write_runtime(directory.path()).expect("runtime");
        assert!(first.join("rlm").join("repl.py").is_file());
        assert_eq!(write_runtime(directory.path()).expect("runtime"), first);
    }

    /// The kernel end to end, when this machine has Python 3.11+: state persists
    /// across cells, `bash()` runs, a host request is answered instead of hanging,
    /// and a cell past its time is interrupted without losing the kernel.
    #[tokio::test]
    async fn the_kernel_keeps_state_and_answers_host_requests() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let shared = Arc::new(ReplShared {
            data_dir: directory.path().to_path_buf(),
            workspace: directory.path().to_path_buf(),
            python_override: None,
            shell: super::default_shell(),
            python: tokio::sync::OnceCell::new(),
            kernel: tokio::sync::Mutex::new(super::KernelSlot::default()),
        });
        if shared.python().await.is_none() {
            eprintln!("skipped: no Python 3.11+ on this machine");
            return;
        }
        let run = |code: &'static str, seconds: u64| {
            let shared = Arc::clone(&shared);
            async move {
                shared
                    .execute(code, Duration::from_secs(seconds), &NoHostRequests)
                    .await
                    .expect("cell")
            }
        };
        assert_eq!(run("x = 41\nprint('hi')\nx + 1", 60).await, "hi\n42");
        assert_eq!(run("x", 60).await, "41", "state persists");
        let spawn = run("await rlm.spawn('task', name='w')", 60).await;
        assert!(
            spawn.contains("rlm.run is not available in ha"),
            "a host request is answered: {spawn}"
        );
        let slow = run("while True: await asyncio.sleep(0.05)", 2).await;
        assert!(slow.contains("interrupted"), "{slow}");
        assert_eq!(
            run("x", 60).await,
            "41",
            "the kernel survived the interrupt"
        );
        if super::default_shell().is_some() {
            let bash = run("(await bash('echo from-bash')).output.strip()", 60).await;
            assert_eq!(bash, "'from-bash'");
        }
    }
}
