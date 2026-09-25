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

/// What one turn's kernel must see: where `rlm.harness` keeps memory (the global
/// state and this conversation's), and the Python skills to pre-import.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KernelContext {
    pub global: PathBuf,
    pub local: PathBuf,
    pub skills: Vec<PythonSkill>,
}

/// A skill that ships a Python package, imported into the kernel by its name.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PythonSkill {
    pub import_name: String,
    /// The directory the package sits in, put on `sys.path`.
    pub src: PathBuf,
}

/// The Python packages a skill directory ships: every `src/<package>/__init__.py`.
#[must_use]
pub fn python_skill_packages(skill_file: &Path) -> Vec<PythonSkill> {
    let Some(src) = skill_file.parent().map(|dir| dir.join("src")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&src) else {
        return Vec::new();
    };
    let mut skills = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("__init__.py").is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let identifier = name
                .chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_');
            identifier.then(|| PythonSkill {
                import_name: name,
                src: src.clone(),
            })
        })
        .collect::<Vec<_>>();
    skills.sort();
    skills
}

/// Printed by the setup cell when a skill failed to import (prime-agent's
/// `PYTHON_SKILL_IMPORT_ERROR_REPORT_MARKER`).
const SKILL_IMPORT_ERRORS_MARKER: &str = "__HA_PYTHON_SKILL_IMPORT_ERRORS__";

impl KernelContext {
    /// The cell that prepares a kernel for this context: `rlm.harness` pointed at the
    /// conversation's memory, and the Python skills imported by name, as prime-agent's
    /// `buildRlmBootstrapCode` does - a module with a callable `run` becomes callable
    /// itself, and a skill that fails to import is replaced by a stub that says why,
    /// so an unavailable skill reaches the model instead of failing the kernel. Paths
    /// are written as JSON strings, which Python reads as the same literals.
    fn setup_code(&self) -> String {
        let quote = |text: &str| serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
        let mut code = format!(
            "import os as _ha_os\n_ha_os.environ[\"RLM_GLOBAL_HARNESS_STATE_DIR\"] = {}\n_ha_os.environ[\"RLM_HARNESS_STATE_DIR\"] = {}\ndel _ha_os\n",
            quote(&self.global.display().to_string()),
            quote(&self.local.display().to_string())
        );
        if self.skills.is_empty() {
            return code;
        }
        let mut paths = self
            .skills
            .iter()
            .map(|skill| skill.src.display().to_string())
            .collect::<Vec<_>>();
        paths.dedup();
        let names = self
            .skills
            .iter()
            .map(|skill| skill.import_name.clone())
            .collect::<Vec<_>>();
        let _ = write!(
            code,
            r#"import importlib as _ha_importlib
import inspect as _ha_inspect
import sys as _ha_sys
import types as _ha_types
for _ha_path in {paths}:
    if _ha_path not in _ha_sys.path:
        _ha_sys.path.insert(0, _ha_path)

class _HaCallableSkillModule(_ha_types.ModuleType):
    async def __call__(self, *args, **kwargs):
        result = self.run(*args, **kwargs)
        if _ha_inspect.isawaitable(result):
            return await result
        return result

class _HaUnavailableSkill:
    def __init__(self, name, error):
        self.__name__ = name
        self._ha_import_error = error
        self.__doc__ = f"Python skill {{name}} is unavailable: {{error}}"

    async def run(self, *args, **kwargs):
        raise RuntimeError(
            f"Python skill {{self.__name__}} is unavailable in this kernel. "
            f"Import error: {{self._ha_import_error}}"
        )

    async def __call__(self, *args, **kwargs):
        return await self.run(*args, **kwargs)

    def __repr__(self):
        return f"<unavailable Python skill {{self.__name__!r}}: {{self._ha_import_error}}>"

def _ha_wrap_skill_module(module):
    run = getattr(module, "run", None)
    if not callable(run) or isinstance(module, _HaCallableSkillModule):
        return module
    wrapped = _HaCallableSkillModule(module.__name__)
    wrapped.__dict__.update(module.__dict__)
    try:
        wrapped.__signature__ = _ha_inspect.signature(run)
    except Exception:
        pass
    doc = getattr(run, "__doc__", None)
    if doc:
        wrapped.__doc__ = doc
    _ha_sys.modules[module.__name__] = wrapped
    return wrapped

_HA_SKILL_IMPORT_ERRORS = {{}}
for _ha_skill_name in {names}:
    try:
        globals()[_ha_skill_name] = _ha_wrap_skill_module(_ha_importlib.import_module(_ha_skill_name))
    except Exception as _ha_skill_error:
        _ha_skill_error_text = str(_ha_skill_error) or type(_ha_skill_error).__name__
        _HA_SKILL_IMPORT_ERRORS[_ha_skill_name] = _ha_skill_error_text
        globals()[_ha_skill_name] = _HaUnavailableSkill(_ha_skill_name, _ha_skill_error_text)

if _HA_SKILL_IMPORT_ERRORS:
    import json as _ha_json
    print({marker} + _ha_json.dumps(_HA_SKILL_IMPORT_ERRORS))
"#,
            paths = serde_json::to_string(&paths).unwrap_or_default(),
            names = serde_json::to_string(&names).unwrap_or_default(),
            marker = serde_json::to_string(SKILL_IMPORT_ERRORS_MARKER).unwrap_or_default(),
        );
        code
    }
}

/// The skills a setup cell reported unavailable, by name.
fn unavailable_skills(output: &str) -> Vec<(String, String)> {
    let Some(at) = output.find(SKILL_IMPORT_ERRORS_MARKER) else {
        return Vec::new();
    };
    let raw = output[at + SKILL_IMPORT_ERRORS_MARKER.len()..]
        .lines()
        .next()
        .unwrap_or_default();
    serde_json::from_str::<serde_json::Map<String, Value>>(raw)
        .map(|errors| {
            errors
                .into_iter()
                .filter_map(|(name, error)| error.as_str().map(|error| (name, error.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

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
    /// Whether any interpreter can be had: found, or buildable with `uv`.
    available: OnceCell<bool>,
    /// The kernel's interpreter, resolved on first use, with what to tell the model
    /// about how it was set up.
    kernel_python: OnceCell<(Option<PathBuf>, Option<String>)>,
    kernel: Mutex<KernelSlot>,
}

#[derive(Default)]
struct KernelSlot {
    kernel: Option<Kernel>,
    /// A kernel ran before and is gone, so the next result says so.
    lost: bool,
    /// How the interpreter was set up was said once.
    announced: bool,
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
            available: OnceCell::new(),
            kernel_python: OnceCell::new(),
            kernel: Mutex::new(KernelSlot::default()),
        }))
    }

    /// A Python 3.11+ found on this machine (or `HA_PYTHON`), found once.
    pub async fn python(&self) -> Option<&PathBuf> {
        self.python
            .get_or_init(|| find_python(self.python_override.clone()))
            .await
            .as_ref()
    }

    /// Whether the REPL can run at all, decided without building anything: an
    /// interpreter is set, the kernel venv exists, `uv` can build it, or a system
    /// Python is found.
    pub async fn available(&self) -> bool {
        *self
            .available
            .get_or_init(|| async {
                self.python_override.is_some()
                    || venv::python(&venv::dir(&self.data_dir)).is_file()
                    || venv::find_uv().await.is_some()
                    || self.python().await.is_some()
            })
            .await
    }

    /// The interpreter the kernel runs on, as prime-agent's `ensureKernelPython`
    /// picks it: `HA_PYTHON` as given; else the kernel venv under the data directory,
    /// built with `uv` - Python 3.11, prime-agent's default packages and what the
    /// bundled skills import - when it is missing or stale; else the system Python,
    /// on which skills whose packages are absent are reported unavailable.
    async fn kernel_python(&self) -> (Option<PathBuf>, Option<String>) {
        self.kernel_python
            .get_or_init(|| async {
                if self.python_override.is_some() {
                    return (self.python().await.cloned(), None);
                }
                let venv_dir = venv::dir(&self.data_dir);
                if venv::ready(&venv_dir) {
                    return (Some(venv::python(&venv_dir)), None);
                }
                let fallback = self.python().await.cloned();
                let Some(uv) = venv::find_uv().await else {
                    return (
                        fallback,
                        Some(format!(
                            "[The kernel runs on the system Python: uv is not installed, so the kernel venv with prime-agent's default packages was not built. Install uv ({}) and restart to get it.]",
                            venv::UV_INSTALL_HINT
                        )),
                    );
                };
                match venv::build(&uv, &venv_dir).await {
                    Ok(()) => (
                        Some(venv::python(&venv_dir)),
                        Some("[The kernel venv was set up with uv (one-time).]".to_owned()),
                    ),
                    Err(error) => (
                        fallback,
                        Some(format!(
                            "[The kernel venv could not be built ({error}); the kernel runs on the system Python.]"
                        )),
                    ),
                }
            })
            .await
            .clone()
    }

    /// Run one cell, starting the kernel when there is none.
    async fn execute(
        &self,
        code: &str,
        timeout: Duration,
        host: &dyn HostRequests,
        harness: Option<&KernelContext>,
    ) -> Result<String, HarnessError> {
        let (python, setup) = self.kernel_python().await;
        let python = python.ok_or_else(|| {
            HarnessError::new(
                ErrorCode::PolicyDenied,
                "no Python 3.11+ interpreter was found for the REPL",
            )
        })?;
        let mut slot = self.kernel.lock().await;
        let mut notice = None;
        if slot.kernel.is_none() {
            if std::mem::take(&mut slot.lost) {
                notice = Some(KERNEL_RESTART_NOTICE.to_owned());
            } else if !std::mem::replace(&mut slot.announced, true) {
                notice = setup;
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
        // The conversation decides which local memory the kernel writes; a kernel that
        // outlives `/new` or `/resume` is pointed at the new one before the cell runs.
        let mut setup_note = None;
        if let Some(context) = harness
            && kernel.harness.as_ref() != Some(context)
        {
            let applied = kernel
                .execute(&context.setup_code(), READY_TIMEOUT, &NoHostRequests)
                .await;
            if let Ok(output) = &applied {
                kernel.harness = Some(context.clone());
                let missing = unavailable_skills(output);
                if !missing.is_empty() {
                    setup_note = Some(format!(
                        "[Python skills unavailable in this kernel: {}]",
                        missing
                            .iter()
                            .map(|(name, error)| format!("{name} ({error})"))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ));
                }
            }
        }
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
        let notes = [notice, setup_note]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        Ok(if notes.is_empty() {
            text
        } else if text.is_empty() {
            notes.join("\n")
        } else {
            format!("{}\n\n{text}", notes.join("\n"))
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
    /// The memory directories this kernel was last pointed at.
    harness: Option<KernelContext>,
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
            harness: None,
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

/// The kernel venv, as prime-agent's `kernel/bootstrap.ts` builds it.
mod venv {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::process::Command;

    /// The Python the venv is built on (prime-agent's `PYTHON_VERSION`).
    const PYTHON_VERSION: &str = "3.11";
    /// `dill` for state snapshots, prime-agent's `DEFAULT_RLM_EXTRA_PACKAGES`, and
    /// the packages the bundled skills import (`pillow` for `attach_image`).
    pub const PACKAGES: [&str; 14] = [
        "dill",
        "requests",
        "httpx",
        "pyyaml",
        "tomli",
        "python-dotenv",
        "pandas",
        "numpy",
        "scipy",
        "beautifulsoup4",
        "lxml",
        "pydantic",
        "tyro",
        "pillow",
    ];
    const MARKER: &str = "ha-kernel.json";
    const STEP_TIMEOUT: Duration = Duration::from_mins(15);
    pub const UV_INSTALL_HINT: &str = "https://docs.astral.sh/uv/getting-started/installation/";

    #[must_use]
    pub fn dir(data_dir: &Path) -> PathBuf {
        data_dir.join("kernel-venv")
    }

    #[must_use]
    pub fn python(venv: &Path) -> PathBuf {
        if cfg!(windows) {
            venv.join("Scripts").join("python.exe")
        } else {
            venv.join("bin").join("python")
        }
    }

    /// What the marker records: a venv built for another list is rebuilt.
    #[must_use]
    pub fn identity() -> String {
        format!("python {PYTHON_VERSION}; {}", PACKAGES.join(" "))
    }

    /// Whether the venv exists and was built for the current package list.
    #[must_use]
    pub fn ready(venv: &Path) -> bool {
        python(venv).is_file()
            && std::fs::read_to_string(venv.join(MARKER))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .is_some_and(|marker| marker["identity"].as_str() == Some(identity().as_str()))
    }

    fn command(program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            command.creation_flags(0x0800_0000);
        }
        command
    }

    /// `uv` on `PATH`, or where its installer puts it (prime-agent's `ensureUv`).
    /// Nothing is installed: an absent `uv` is reported, not fetched.
    pub async fn find_uv() -> Option<PathBuf> {
        let mut candidates = vec![PathBuf::from("uv")];
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            let name = if cfg!(windows) { "uv.exe" } else { "uv" };
            candidates.push(PathBuf::from(home).join(".local").join("bin").join(name));
        }
        for candidate in candidates {
            let ran = tokio::time::timeout(
                Duration::from_secs(10),
                command(&candidate).arg("--version").status(),
            )
            .await;
            if matches!(ran, Ok(Ok(status)) if status.success()) {
                return Some(candidate);
            }
        }
        None
    }

    async fn run(uv: &Path, args: &[&str]) -> Result<(), String> {
        let output = tokio::time::timeout(STEP_TIMEOUT, command(uv).args(args).output())
            .await
            .map_err(|_| format!("uv {} timed out", args.first().unwrap_or(&"")))?
            .map_err(|error| format!("uv could not run: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().rev().take(3).collect::<Vec<_>>();
        Err(format!(
            "uv {} failed: {}",
            args.first().unwrap_or(&""),
            tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
        ))
    }

    /// Build (or rebuild) the venv: the Python, the venv unseeded, then every
    /// package through `uv pip install --python`, and the marker last, so a build
    /// cut short is rebuilt next time.
    pub async fn build(uv: &Path, venv: &Path) -> Result<(), String> {
        if venv.exists() {
            std::fs::remove_dir_all(venv)
                .map_err(|error| format!("the old venv could not be removed: {error}"))?;
        }
        if let Some(parent) = venv.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let venv_text = venv.display().to_string();
        run(uv, &["python", "install", PYTHON_VERSION]).await?;
        run(uv, &["venv", &venv_text, "--python", PYTHON_VERSION]).await?;
        let python_text = python(venv).display().to_string();
        let mut install = vec!["pip", "install", "--python", python_text.as_str()];
        install.extend(PACKAGES);
        run(uv, &install).await?;
        std::fs::write(
            venv.join(MARKER),
            serde_json::json!({ "identity": identity() }).to_string(),
        )
        .map_err(|error| format!("the venv marker could not be written: {error}"))
    }
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
    harness: KernelContext,
}

impl ReplHost {
    /// The tool for this turn, when an interpreter exists.
    pub async fn for_turn(
        shared: &Arc<ReplShared>,
        host: Arc<dyn HostRequests>,
        harness: KernelContext,
    ) -> Option<Self> {
        if !shared.available().await {
            return None;
        }
        Some(Self {
            shared: Arc::clone(shared),
            host,
            harness,
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
                    Some(&self.harness),
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
    fn a_venv_is_ready_only_when_built_for_the_current_packages() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let venv = super::venv::dir(directory.path());
        assert!(!super::venv::ready(&venv));
        let python = super::venv::python(&venv);
        std::fs::create_dir_all(python.parent().expect("parent")).expect("dirs");
        std::fs::write(&python, "").expect("python");
        std::fs::write(venv.join("ha-kernel.json"), r#"{"identity": "old list"}"#).expect("marker");
        assert!(!super::venv::ready(&venv), "a stale marker is rebuilt");
        std::fs::write(
            venv.join("ha-kernel.json"),
            serde_json::json!({ "identity": super::venv::identity() }).to_string(),
        )
        .expect("marker");
        assert!(super::venv::ready(&venv));
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
            available: tokio::sync::OnceCell::new(),
            kernel_python: tokio::sync::OnceCell::new(),
            kernel: tokio::sync::Mutex::new(super::KernelSlot::default()),
        });
        if shared.python().await.is_none() {
            eprintln!("skipped: no Python 3.11+ on this machine");
            return;
        }
        // One skill that imports and one that does not, as the vendored skills can be.
        let skills_root = directory.path().join("skills");
        for (name, body) in [
            (
                "hello_skill",
                "async def run(name):\n    return f'hello {name}'\n",
            ),
            ("broken_skill", "import not_a_module_anywhere\n"),
        ] {
            let package = skills_root.join("src").join(name);
            std::fs::create_dir_all(&package).expect("package");
            std::fs::write(package.join("__init__.py"), body).expect("init");
        }
        std::fs::write(skills_root.join("SKILL.md"), "---\nname: hello\n---\n").expect("skill");
        let dirs = super::KernelContext {
            global: directory.path().join("global-harness"),
            local: directory.path().join("local-harness"),
            skills: super::python_skill_packages(&skills_root.join("SKILL.md")),
        };
        assert_eq!(dirs.skills.len(), 2);
        let run = |code: &'static str, seconds: u64| {
            let shared = Arc::clone(&shared);
            let dirs = dirs.clone();
            async move {
                shared
                    .execute(
                        code,
                        Duration::from_secs(seconds),
                        &NoHostRequests,
                        Some(&dirs),
                    )
                    .await
                    .expect("cell")
            }
        };
        let first = run("x = 41\nprint('hi')\nx + 1", 60).await;
        assert!(
            first.contains("[Python skills unavailable in this kernel: broken_skill (")
                && first.ends_with("hi\n42"),
            "an unavailable skill is reported once, before the first output: {first}"
        );
        assert_eq!(
            run("await hello_skill('ha')", 60).await,
            "'hello ha'",
            "a skill module with run() is callable"
        );
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
        // Memory written through `rlm.harness` lands in the conversation's files, where
        // the host reads its digest from.
        let created = run(
            "rlm.harness.create_memory('Indentation', 'The user prefers tabs').id",
            60,
        )
        .await;
        assert!(!created.contains("Traceback"), "{created}");
        let state = crate::interactive::harness::load(&dirs.local, "local");
        let memories = state.entries.get("memory").expect("memories");
        assert_eq!(memories.len(), 1, "{created}");
        assert_eq!(
            memories[0].content.as_deref(),
            Some("The user prefers tabs")
        );
        if super::default_shell().is_some() {
            let bash = run("(await bash('echo from-bash')).output.strip()", 60).await;
            assert_eq!(bash, "'from-bash'");
        }
    }
}
