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
//! - every `host_request` the runtime sends is answered - by the latest turn's
//!   handler when it knows the request, with an error otherwise - because an
//!   unanswered request would leave the cell waiting forever. Requests are read by
//!   a pump that runs for the kernel's whole life and answered concurrently, so a
//!   detached task (an `rlm.spawn` finishing after its cell went idle) is answered
//!   too, as prime-agent's late handlers are;
//! - the namespace is snapshotted to the conversation's kernel directory a moment
//!   after a cell did work and once more when the kernel is disposed (app exit, or
//!   the next cell belonging to another conversation), and revived when a kernel
//!   starts for that conversation again (prime-agent's `kernel/state-snapshot.ts`);
//! - the kernel's stderr is kept as an 8 KB tail for errors and an owner-only log,
//!   a kernel is stopped with the protocol's `shutdown` before it is killed, and
//!   every kernel is journaled so one a crashed `ha` left behind is reaped
//!   (prime-agent's `orphan-process-journal.ts`).
//!
//! Running Python is running code, so the tool passes the same approval as
//! `run_shell`. It is offered only when a Python 3.11+ interpreter is found
//! (`HA_PYTHON`, else `python3`/`python`/`py -3` on `PATH`); `HA_REPL=off` removes it.

mod journal;
mod stderr;

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use harness_tools::{
    CodingToolAction, ExternalToolCatalog, ExternalToolDispatcher, ExternalTools,
    ToolDispatchAuthorization, ToolOutput,
};
use harness_types::{ErrorCode, HarnessError};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OnceCell, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::paths::LaunchEnvironment;

/// `off` (or `0`, `false`, `no`) removes the REPL.
pub const REPL_VARIABLE: &str = "HA_REPL";
/// An explicit interpreter for the kernel.
pub const PYTHON_VARIABLE: &str = "HA_PYTHON";
/// An explicit `uv` for building the kernel venv, looked at before any other.
pub const UV_VARIABLE: &str = "HA_UV";
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
/// prime-agent's `MAX_PROTOCOL_LINE_CHARS`: the largest legitimate frame is an image
/// display; a line that cannot end within this is corruption, not output to buffer.
const MAX_PROTOCOL_LINE_BYTES: usize = 32 * 1024 * 1024;
/// prime-agent's `MAX_BACKGROUND_OUTPUT_CHARS`: unattributed output kept between and
/// during cells.
const MAX_BACKGROUND_OUTPUT_CHARS: usize = 64 * 1024;
/// prime-agent's `MAX_HANDLED_HOST_REQUEST_IDS`: ids never repeat; the bound only
/// keeps a misbehaving runtime from growing the set forever.
const MAX_HANDLED_HOST_REQUEST_IDS: usize = 1024;
/// How much of the stderr tail an error quotes (prime-agent quotes the last 1024).
const STDERR_QUOTE_CHARS: usize = 1024;
/// prime-agent's `KERNEL_SHUTDOWN_TIMEOUT_MS`.
const KERNEL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// prime-agent's `HOST_REQUEST_SHUTDOWN_TIMEOUT_MS`.
const HOST_REQUEST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
/// prime-agent's `DEFAULT_SNAPSHOT_DEBOUNCE_MS`.
const SNAPSHOT_DEBOUNCE: Duration = Duration::from_millis(1500);
/// prime-agent's `SNAPSHOT_EXECUTION_TIMEOUT_MS`.
const SNAPSHOT_EXECUTION_TIMEOUT: Duration = Duration::from_secs(5);
/// prime-agent's `RESTORE_EXECUTION_TIMEOUT_MS`.
const RESTORE_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30);
/// prime-agent's `DEFAULT_SNAPSHOT_MAX_BYTES`: the whole payload.
const SNAPSHOT_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// prime-agent's `DEFAULT_SNAPSHOT_MAX_VARIABLE_BYTES`: one serialized variable.
const SNAPSHOT_MAX_VARIABLE_BYTES: u64 = 16 * 1024 * 1024;
/// The complete event vocabulary of the protocol; the version handshake is exact,
/// so an unknown kind is corruption, not a newer runtime.
const PROTOCOL_EVENT_KINDS: [&str; 8] = [
    "ready",
    "stdout",
    "stderr",
    "result",
    "display",
    "host_request",
    "error",
    "done",
];

/// The vendored runtime, embedded so a single binary carries it.
const RUNTIME_FILES: [(&str, &str); 7] = [
    ("__init__.py", include_str!("../../python/rlm/__init__.py")),
    ("repl.py", include_str!("../../python/rlm/repl.py")),
    ("bash.py", include_str!("../../python/rlm/bash.py")),
    ("_winjob.py", include_str!("../../python/rlm/_winjob.py")),
    ("harness.py", include_str!("../../python/rlm/harness.py")),
    ("mcp.py", include_str!("../../python/rlm/mcp.py")),
    ("mcp_base.py", include_str!("../../python/rlm/mcp_base.py")),
];

/// prime-agent's `ATTACHMENT_DISPLAY_MIME`: `{mime_type, data, path}` for one image.
const ATTACHMENT_DISPLAY_MIME: &str = "application/vnd.prime-agent.attachment+json";

/// What one cell returned: its text and the images it loaded for the model.
#[derive(Debug, Default)]
pub struct CellOutput {
    pub text: String,
    pub images: Vec<Value>,
}

/// Code every new kernel runs first, as prime-agent's bootstrap cell does: the
/// `rlm` namespace and `bash` are globals, and output has no colour codes.
const BOOTSTRAP_CODE: &str = "import asyncio\n\
import os as _ha_os\n\
_ha_os.environ[\"NO_COLOR\"] = \"1\"\n\
import rlm as _ha_rlm_module\n\
rlm = _ha_rlm_module.rlm\n\
bash = _ha_rlm_module.bash\n\
import rlm.mcp as mcp\n\
del _ha_os";

/// The answer to one host request: `None` when the type has no handler.
pub type HostReply<'a> = Pin<Box<dyn Future<Output = Option<Result<Value, String>>> + Send + 'a>>;

/// What one turn's kernel must see: where `rlm.harness` keeps memory (the global
/// state and this conversation's), the Python skills to pre-import, and the
/// conversation's kernel directory, where its namespace snapshot and stderr log
/// live (prime-agent's per-session artifact directory). An empty `kernel_dir`
/// keeps no snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct KernelContext {
    pub global: PathBuf,
    pub local: PathBuf,
    pub skills: Vec<PythonSkill>,
    pub kernel_dir: PathBuf,
}

/// Where a conversation's kernel lives on disk, under the data directory.
#[must_use]
pub fn kernel_dir(data_dir: &Path, conversation: &str) -> PathBuf {
    data_dir.join("sessions").join(conversation).join("kernel")
}

/// The files of one conversation's namespace snapshot (prime-agent's
/// `snapshotPathIn` / `manifestPathIn`).
#[derive(Clone, Debug, Eq, PartialEq)]
struct SnapshotTarget {
    dir: PathBuf,
}

impl SnapshotTarget {
    fn of(context: &KernelContext) -> Option<Self> {
        (!context.kernel_dir.as_os_str().is_empty()).then(|| Self {
            dir: context.kernel_dir.clone(),
        })
    }

    fn payload(&self) -> PathBuf {
        self.dir.join("kernel-state.dill")
    }

    fn manifest(&self) -> PathBuf {
        self.dir.join("kernel-state.json")
    }

    /// The protocol's `snapshot` request, with prime-agent's caps; `prune` also
    /// removes the live variables over the per-variable cap.
    fn request(&self, prune: bool) -> Value {
        json!({
            "type": "snapshot",
            "path": self.payload().display().to_string(),
            "manifest_path": self.manifest().display().to_string(),
            "max_bytes": SNAPSHOT_MAX_BYTES,
            "max_variable_bytes": SNAPSHOT_MAX_VARIABLE_BYTES,
            "prune_oversized": prune,
        })
    }
}

/// What one snapshot left out (prime-agent's `SnapshotResult`): the names it could not
/// serialize, with why, and the live variables a pruning snapshot removed.
#[derive(Debug, Default, Eq, PartialEq)]
struct SnapshotResult {
    skipped: Vec<(String, String)>,
    pruned: Vec<String>,
}

/// What one restore revived (prime-agent's `RestoreResult`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RestoreResult {
    restored: Vec<String>,
    failed: Vec<(String, String)>,
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn reasons(value: &Value) -> Vec<(String, String)> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some((
                        item["name"].as_str()?.to_owned(),
                        item["reason"].as_str().unwrap_or_default().to_owned(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

impl SnapshotResult {
    fn from_done(done: &Value) -> Self {
        Self {
            skipped: reasons(&done["skipped"]),
            pruned: strings(&done["pruned"]),
        }
    }
}

impl RestoreResult {
    fn from_done(done: &Value) -> Self {
        Self {
            restored: strings(&done["restored"]),
            failed: reasons(&done["failed"]),
        }
    }
}

/// What a new kernel did with its conversation's snapshot.
#[derive(Debug)]
enum Revival {
    /// There was none.
    NoSnapshot,
    /// There was one, and it could not be revived.
    Failed,
    Restored(RestoreResult),
}

/// What the model is told about a kernel that started over: prime-agent's
/// `[python-state-restored]` message when a snapshot was there to revive, the
/// restart notice when a kernel was lost and nothing came back, nothing otherwise.
fn start_notice(lost: bool, revival: &Revival) -> Option<String> {
    let restore = match revival {
        Revival::NoSnapshot => return lost.then(|| KERNEL_RESTART_NOTICE.to_owned()),
        Revival::Failed => None,
        Revival::Restored(result) => Some(result),
    };
    let mut lines = Vec::new();
    match restore {
        Some(result) if !result.restored.is_empty() => lines.push(if lost {
            format!(
                "The Python kernel was restarted; its state was revived from the last snapshot, so anything defined after it is gone. These names are available again: {}.",
                result.restored.join(", ")
            )
        } else {
            format!(
                "Your Python kernel state was revived from your previous session. These names are available again: {}.",
                result.restored.join(", ")
            )
        }),
        _ => lines.push(
            "Your previous Python kernel state could not be revived; the kernel is starting fresh, so re-create any variables, imports, or loaded data you need."
                .to_owned(),
        ),
    }
    if let Some(result) = restore
        && !result.failed.is_empty()
    {
        lines.push(format!(
            "These could not be restored and must be recreated if needed: {}.",
            result
                .failed
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Some(format!("[python-state-restored] {}", lines.join(" ")))
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
#[cfg(test)]
pub struct NoHostRequests;

#[cfg(test)]
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
    uv_override: Option<PathBuf>,
    shell: Option<PathBuf>,
    python: OnceCell<Option<PathBuf>>,
    /// Whether any interpreter can be had: found, or buildable with `uv`.
    available: OnceCell<bool>,
    /// The kernel's interpreter, resolved on first use, with what to tell the model
    /// about how it was set up.
    kernel_python: OnceCell<(Option<PathBuf>, Option<String>)>,
    kernel: Mutex<KernelSlot>,
    /// Bumped by every scheduled snapshot, so only the last of a burst runs.
    snapshot_epoch: AtomicU64,
    snapshot_debounce: Duration,
    /// The running kernel's link, reachable without the slot's lock, so disposing
    /// can interrupt a cell that holds it.
    link: std::sync::Mutex<Option<Arc<Link>>>,
    /// The journals a crashed `ha` left were reaped once for this app session.
    reaped: AtomicBool,
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
    fn new(
        data_dir: &Path,
        workspace: &Path,
        python_override: Option<String>,
        shell: Option<PathBuf>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            workspace: workspace.to_path_buf(),
            python_override,
            uv_override: None,
            shell,
            python: OnceCell::new(),
            available: OnceCell::new(),
            kernel_python: OnceCell::new(),
            kernel: Mutex::new(KernelSlot::default()),
            snapshot_epoch: AtomicU64::new(0),
            snapshot_debounce: SNAPSHOT_DEBOUNCE,
            link: std::sync::Mutex::new(None),
            reaped: AtomicBool::new(false),
        }
    }

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
        let mut shared = Self::new(data_dir, workspace, value(PYTHON_VARIABLE), shell);
        shared.uv_override = value(UV_VARIABLE).map(PathBuf::from);
        Some(Arc::new(shared))
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
                    || venv::find_uv(self.uv_override.clone()).await.is_some()
                    || self.python().await.is_some()
            })
            .await
    }

    /// The interpreter the kernel runs on, as prime-agent's `ensureKernelPython`
    /// picks it: `HA_PYTHON` as given; else the kernel venv under the data directory,
    /// with prime-agent's default packages and what the bundled skills import, built
    /// when it is missing or stale, with `uv` first and the system Python's own
    /// `venv` and `pip` when `uv` is missing or fails; else the bare system Python,
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
                let system = self.python().await.cloned();
                let uv = venv::find_uv(self.uv_override.clone()).await;
                let uv_found = uv.is_some();
                let mut failures = Vec::new();
                for builder in venv::builders(uv, system.clone()) {
                    match venv::build(&builder, &venv_dir).await {
                        Ok(()) => {
                            return (
                                Some(venv::python(&venv_dir)),
                                Some(venv::built_notice(&builder, uv_found, &failures)),
                            );
                        }
                        Err(error) => failures.push(error),
                    }
                }
                (system, Some(venv::fallback_notice(uv_found, &failures)))
            })
            .await
            .clone()
    }

    /// This process's orphan journal.
    fn journal(&self) -> PathBuf {
        journal::path_for(&journal::dir(&self.data_dir), std::process::id())
    }

    fn set_link(&self, link: Option<Arc<Link>>) {
        *self
            .link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = link;
    }

    /// Drop a kernel that is gone or unusable; the next result says the state went
    /// with it.
    fn lose(&self, slot: &mut KernelSlot) {
        slot.kernel = None;
        slot.lost = true;
        self.set_link(None);
    }

    /// Run one cell, starting the kernel when there is none.
    async fn execute(
        self: &Arc<Self>,
        code: &str,
        timeout: Duration,
        host: Arc<dyn HostRequests>,
        harness: Option<&KernelContext>,
    ) -> Result<CellOutput, HarnessError> {
        let (python, setup) = self.kernel_python().await;
        let python = python.ok_or_else(|| {
            HarnessError::new(
                ErrorCode::PolicyDenied,
                "no Python 3.11+ interpreter was found for the REPL",
            )
        })?;
        let target = harness.and_then(SnapshotTarget::of);
        let mut slot = self.kernel.lock().await;
        // A kernel whose protocol stream ended - it exited, or wrote something that
        // is not the protocol - is gone; the next one revives the last snapshot.
        if slot
            .kernel
            .as_ref()
            .is_some_and(|kernel| !kernel.link.is_open())
        {
            self.lose(&mut slot);
        }
        // A kernel belongs to one conversation, as prime-agent's does to its session:
        // another conversation's first cell disposes it, with its final snapshot, and
        // starts that conversation's own kernel.
        if let Some(previous) = slot.kernel.take_if(|kernel| kernel.snapshot != target) {
            self.set_link(None);
            previous.shutdown(true).await;
        }
        let notice = if slot.kernel.is_none() {
            self.start_kernel(&mut slot, &python, setup, target).await?
        } else {
            None
        };
        let kernel = slot.kernel.as_mut().expect("the kernel was just started");
        // Host requests - a cell's own and those of tasks it left running - are
        // answered by the latest turn's handlers.
        kernel.link.set_host(Arc::clone(&host));
        // The conversation decides which local memory the kernel writes and which
        // skills it imports; a change is applied before the cell runs.
        let mut setup_note = None;
        if let Some(context) = harness
            && kernel.harness.as_ref() != Some(context)
        {
            let applied = kernel
                .execute(&context.setup_code(), READY_TIMEOUT, false)
                .await;
            if let Ok((output, _)) = &applied {
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
        let outcome = kernel.execute(code, timeout, true).await;
        let images = std::mem::take(&mut kernel.images);
        let (text, worked) = match outcome {
            Ok(cell) => cell,
            Err(dead) => {
                // The process is gone or stuck: drop it, and the next call starts over.
                self.lose(&mut slot);
                (dead, false)
            }
        };
        drop(slot);
        // Refresh the snapshot after real work, so a later start (or a crash before a
        // clean exit) revives the latest namespace.
        if worked {
            self.schedule_snapshot();
        }
        let notes = [notice, setup_note]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let text = if notes.is_empty() {
            text
        } else if text.is_empty() {
            notes.join("\n")
        } else {
            format!("{}\n\n{text}", notes.join("\n"))
        };
        Ok(CellOutput { text, images })
    }

    /// Start the conversation's kernel into `slot`: spawned, its snapshot revived,
    /// the runtime bootstrapped. What comes back is what the model is told first.
    async fn start_kernel(
        &self,
        slot: &mut KernelSlot,
        python: &Path,
        setup: Option<String>,
        target: Option<SnapshotTarget>,
    ) -> Result<Option<String>, HarnessError> {
        let lost = std::mem::take(&mut slot.lost);
        let setup = (!lost && !std::mem::replace(&mut slot.announced, true))
            .then_some(setup)
            .flatten();
        self.reap_stale_journals();
        let runtime = write_runtime(&self.data_dir)
            .map_err(|error| HarnessError::new(ErrorCode::StorageWriteFailed, error))?;
        let mut kernel = Kernel::start(
            python,
            &runtime,
            &self.workspace,
            &self.data_dir,
            self.shell.as_deref(),
            target,
            self.journal(),
        )
        .await
        .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
        let revival = kernel
            .provision()
            .await
            .map_err(|error| HarnessError::new(ErrorCode::InvalidPayload, error))?;
        self.set_link(Some(Arc::clone(&kernel.link)));
        slot.kernel = Some(kernel);
        Ok(match (setup, start_notice(lost, &revival)) {
            (Some(setup), Some(start)) => Some(format!("{setup}\n{start}")),
            (setup, start) => setup.or(start),
        })
    }

    /// prime-agent's `scheduleSnapshot`: one snapshot a moment after the last cell of
    /// a burst, taken when no cell holds the kernel.
    fn schedule_snapshot(self: &Arc<Self>) {
        let epoch = self.snapshot_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let this = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(this.snapshot_debounce).await;
            if this.snapshot_epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            let mut slot = this.kernel.lock().await;
            if this.snapshot_epoch.load(Ordering::SeqCst) != epoch {
                return;
            }
            let Some(kernel) = slot.kernel.as_mut() else {
                return;
            };
            // A cell a canceled turn left running is settled by the next cell.
            if kernel.in_flight.is_some() || !kernel.link.is_open() {
                return;
            }
            if kernel.snapshot(false).await.is_err() {
                this.lose(&mut slot);
            }
        });
    }

    /// A turn's handlers hold that turn's store. The kernel outlives the turn, so
    /// the turn takes them back when it ends: otherwise the store never closes and
    /// the next turn finds the project locked (`writer_locked`). A request a task
    /// makes between turns is answered as having no host, until the next cell.
    pub fn release_host(&self) {
        let link = self
            .link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(link) = link {
            link.state().host = None;
        }
    }

    /// Dispose the kernel as prime-agent's session teardown does: a cell still
    /// running is interrupted, the namespace gets its final snapshot, host requests in
    /// flight get a moment, and the kernel is asked to shut down before it is
    /// killed. The next cell starts a kernel again.
    pub async fn dispose(&self) {
        // The final snapshot supersedes a pending one.
        self.snapshot_epoch.fetch_add(1, Ordering::SeqCst);
        let link = self
            .link
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(link) = link {
            link.interrupt_active().await;
        }
        let Ok(mut slot) =
            tokio::time::timeout(SNAPSHOT_EXECUTION_TIMEOUT, self.kernel.lock()).await
        else {
            return;
        };
        if let Some(kernel) = slot.kernel.take() {
            self.set_link(None);
            kernel.shutdown(true).await;
        }
        drop(slot);
        journal::clear_if_idle(&self.journal(), std::process::id());
    }

    /// prime-agent's `pruneOversizedVariables`, run after a compaction: the namespace
    /// is snapshotted and the variables over the per-variable cap are removed from
    /// the live kernel. `None` when no kernel runs or the snapshot failed.
    pub async fn prune_oversized_variables(&self) -> Option<Vec<String>> {
        let mut slot = self.kernel.lock().await;
        let kernel = slot.kernel.as_mut()?;
        if !kernel.link.is_open() || kernel.in_flight.is_some() {
            return None;
        }
        let Ok(result) = kernel.snapshot(true).await else {
            self.lose(&mut slot);
            return None;
        };
        result.map(|result| result.pruned)
    }

    /// Reap, once per app session, the kernels a crashed `ha` left running.
    fn reap_stale_journals(&self) {
        if self.reaped.swap(true, Ordering::SeqCst) {
            return;
        }
        let dir = journal::dir(&self.data_dir);
        tokio::spawn(async move {
            journal::reap_stale(&dir, std::process::id()).await;
        });
    }
}

/// The host's end of one kernel's protocol, shared by the kernel, the pump that
/// reads its events for its whole life, and the tasks answering its host requests.
struct Link {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    state: std::sync::Mutex<LinkState>,
    /// How many host requests are being answered, so shutdown can wait for them.
    host_tasks: watch::Sender<usize>,
}

#[derive(Default)]
struct LinkState {
    /// Where each request's events go, by request id, until its `done`.
    routes: HashMap<String, mpsc::UnboundedSender<Value>>,
    /// The request the kernel is running.
    active: Option<String>,
    /// Output no running request owns: user threads, a finished cell's tasks.
    background: String,
    background_chars: usize,
    background_truncated: bool,
    /// Host requests already started, oldest first.
    handled: VecDeque<String>,
    handled_ids: HashSet<String>,
    /// The latest turn's handlers.
    host: Option<Arc<dyn HostRequests>>,
    stderr: stderr::Tail,
    /// Why the protocol stream ended, once it has.
    closed: Option<String>,
}

impl LinkState {
    fn diagnostic(&mut self, message: &str) {
        self.stderr
            .push(&format!("[kernel] {}\n", message.trim_end()));
    }

    fn append_background(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let room = MAX_BACKGROUND_OUTPUT_CHARS.saturating_sub(self.background_chars);
        let count = text.chars().count();
        if count > room {
            self.background.extend(text.chars().take(room));
            self.background_chars += room;
            self.background_truncated = true;
        } else {
            self.background.push_str(text);
            self.background_chars += count;
        }
    }

    fn take_background(&mut self) -> String {
        let mut text = std::mem::take(&mut self.background);
        self.background_chars = 0;
        if std::mem::take(&mut self.background_truncated) {
            let _ = write!(
                text,
                "\n[... background output truncated at {MAX_BACKGROUND_OUTPUT_CHARS} chars ...]"
            );
        }
        text
    }

    /// Whether a host request is new; the set of seen ids stays bounded.
    fn remember(&mut self, id: &str) -> bool {
        if !self.handled_ids.insert(id.to_owned()) {
            return false;
        }
        self.handled.push_back(id.to_owned());
        while self.handled.len() > MAX_HANDLED_HOST_REQUEST_IDS {
            if let Some(oldest) = self.handled.pop_front() {
                self.handled_ids.remove(&oldest);
            }
        }
        true
    }
}

impl Link {
    fn new(writer: Box<dyn AsyncWrite + Send + Unpin>) -> Self {
        Self {
            writer: Mutex::new(writer),
            state: std::sync::Mutex::new(LinkState::default()),
            host_tasks: watch::Sender::new(0),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LinkState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn write(&self, request: &Value) -> Result<(), String> {
        let mut line = request.to_string();
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|error| format!("the Python kernel stopped: {error}"))?;
        writer
            .flush()
            .await
            .map_err(|error| format!("the Python kernel stopped: {error}"))
    }

    fn is_open(&self) -> bool {
        self.state().closed.is_none()
    }

    /// Start collecting the events of request `id`.
    fn route(&self, id: &str) -> Result<mpsc::UnboundedReceiver<Value>, String> {
        let mut state = self.state();
        if let Some(reason) = &state.closed {
            return Err(format!("[{reason}]"));
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        state.routes.insert(id.to_owned(), sender);
        Ok(receiver)
    }

    /// The stream is over: every waiting request sees it end.
    fn close(&self, reason: String) {
        let mut state = self.state();
        if state.closed.is_none() {
            state.closed = Some(reason);
        }
        state.routes.clear();
    }

    fn diagnostic(&self, message: &str) {
        self.state().diagnostic(message);
    }

    fn set_host(&self, host: Arc<dyn HostRequests>) {
        self.state().host = Some(host);
    }

    fn take_background(&self) -> String {
        self.state().take_background()
    }

    /// Interrupt the request the kernel is running, if any.
    async fn interrupt_active(&self) {
        let active = self.state().active.clone();
        if let Some(id) = active {
            let _ = self.write(&json!({"type": "interrupt", "id": id})).await;
        }
    }
}

/// What one read of the protocol stream found.
#[derive(Debug, Eq, PartialEq)]
enum Frame {
    Line,
    Eof,
    /// The line outgrew the cap before it ended.
    TooLong,
}

/// Read one line into `line` (without its newline), refusing to buffer more than
/// `cap` bytes of it.
async fn read_frame(
    reader: &mut (impl AsyncBufRead + Unpin),
    line: &mut Vec<u8>,
    cap: usize,
) -> std::io::Result<Frame> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if line.is_empty() {
                Frame::Eof
            } else {
                Frame::Line
            });
        }
        if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
            if line.len() + end > cap {
                return Ok(Frame::TooLong);
            }
            line.extend_from_slice(&available[..end]);
            reader.consume(end + 1);
            return Ok(Frame::Line);
        }
        let length = available.len();
        if line.len() + length > cap {
            return Ok(Frame::TooLong);
        }
        line.extend_from_slice(available);
        reader.consume(length);
    }
}

/// Why a JSON object is still not a protocol frame (prime-agent's
/// `invalidProtocolFrameReason`): `done` and `host_request` route strictly by id, and
/// dropping an id-less one would leave its request waiting forever.
fn invalid_frame_reason(event: &Value) -> Option<String> {
    let Some(kind) = event
        .get("event")
        .and_then(Value::as_str)
        .filter(|kind| PROTOCOL_EVENT_KINDS.contains(kind))
    else {
        return Some("unknown protocol event".to_owned());
    };
    let has_id = event
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty());
    (matches!(kind, "done" | "host_request") && !has_id).then(|| format!("{kind} frame without id"))
}

fn preview(line: &str) -> String {
    line.chars().take(200).collect()
}

/// The stream carried something that is not the protocol: the kernel is unusable,
/// and the next call starts a new one that revives the last snapshot, as prime-agent's
/// protocol repair does.
fn protocol_error(link: &Link, diagnostic: &str) {
    link.diagnostic(diagnostic);
    link.close(format!("Kernel protocol error: {diagnostic}"));
}

/// Read the kernel's events for its whole life: `ready` to the starter, host
/// requests to their own tasks, a request's events to it, and output nobody owns
/// to the background buffer.
async fn pump(
    link: Arc<Link>,
    reader: impl AsyncRead + Send + Unpin + 'static,
    ready: oneshot::Sender<Value>,
) {
    let mut reader = BufReader::new(reader);
    let mut ready = Some(ready);
    let mut bytes = Vec::new();
    loop {
        bytes.clear();
        match read_frame(&mut reader, &mut bytes, MAX_PROTOCOL_LINE_BYTES).await {
            Ok(Frame::Line) => {}
            Ok(Frame::Eof) | Err(_) => {
                link.close("The Python kernel exited.".to_owned());
                return;
            }
            Ok(Frame::TooLong) => {
                protocol_error(
                    &link,
                    &format!("oversized protocol line: exceeds {MAX_PROTOCOL_LINE_BYTES} bytes"),
                );
                return;
            }
        }
        let text = String::from_utf8_lossy(&bytes);
        if text.trim().is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<Value>(&text) {
            Ok(event) if event.is_object() => event,
            Ok(_) => {
                protocol_error(
                    &link,
                    &format!("non-object protocol line: {}", preview(&text)),
                );
                return;
            }
            Err(_) => {
                protocol_error(
                    &link,
                    &format!("unparseable protocol line: {}", preview(&text)),
                );
                return;
            }
        };
        if let Some(reason) = invalid_frame_reason(&event) {
            protocol_error(&link, &format!("{reason}: {}", preview(&text)));
            return;
        }
        dispatch(&link, event, &mut ready);
    }
}

fn dispatch(link: &Arc<Link>, event: Value, ready: &mut Option<oneshot::Sender<Value>>) {
    let kind = event["event"].as_str().unwrap_or_default().to_owned();
    match kind.as_str() {
        "ready" => {
            if let Some(ready) = ready.take() {
                let _ = ready.send(event);
            }
            return;
        }
        "host_request" => {
            start_host_request(link, &event);
            return;
        }
        _ => {}
    }
    let id = event.get("id").and_then(Value::as_str).map(str::to_owned);
    let mut state = link.state();
    if let Some(id) = &id
        && let Some(route) = state.routes.get(id)
    {
        let _ = route.send(event);
        if kind == "done" {
            state.routes.remove(id);
        }
        return;
    }
    match kind.as_str() {
        // Unowned output (no id, or a finished request's): never merged into a
        // running cell's streams, surfaced as background output instead.
        "stdout" | "stderr" => {
            let text = event["text"].as_str().unwrap_or_default();
            state.append_background(text);
        }
        "error" if id.is_none() => {
            let message = format!(
                "protocol error: {}",
                event["evalue"].as_str().unwrap_or_default()
            );
            state.diagnostic(&message);
        }
        _ => {}
    }
}

/// One host request being answered; shutdown waits until none is.
struct HostTask {
    link: Arc<Link>,
}

impl HostTask {
    fn new(link: Arc<Link>) -> Self {
        link.host_tasks.send_modify(|count| *count += 1);
        Self { link }
    }
}

impl Drop for HostTask {
    fn drop(&mut self) {
        self.link
            .host_tasks
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

/// prime-agent's `startHostRequest`: each request is answered on its own task, so
/// a slow one never holds up another or the cell's own output, and one sent after
/// its cell went idle is answered all the same. A repeated id is answered once.
fn start_host_request(link: &Arc<Link>, event: &Value) {
    let Some(id) = event["id"].as_str().map(str::to_owned) else {
        return;
    };
    let host = {
        let mut state = link.state();
        if !state.remember(&id) {
            return;
        }
        state.host.clone()
    };
    let data = event.get("data").cloned().unwrap_or(Value::Null);
    let task = HostTask::new(Arc::clone(link));
    tokio::spawn(async move {
        let reply = answer_host_request(host.as_deref(), &data).await;
        if let Err(error) = task
            .link
            .write(&json!({"type": "host_reply", "id": id, "data": reply}))
            .await
        {
            task.link.diagnostic(&format!(
                "failed to send host request reply for {id}: {error}"
            ));
        }
        drop(task);
    });
}

async fn answer_host_request(host: Option<&dyn HostRequests>, data: &Value) -> Value {
    let Some(kind) = data
        .get("type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
    else {
        let error = if data.is_object() {
            "host request payload must have a string type"
        } else {
            "host request payload must be an object"
        };
        return json!({"status": "error", "error": error});
    };
    let answer = match host {
        Some(host) => host.handle(data).await,
        None => None,
    };
    match answer {
        Some(Ok(result)) => json!({"status": "ok", "result": result}),
        Some(Err(error)) => json!({"status": "error", "error": error}),
        None => json!({"status": "error", "error": format!("{kind} is not available in ha")}),
    }
}

/// One running kernel process.
struct Kernel {
    child: Option<Child>,
    pid: Option<u32>,
    link: Arc<Link>,
    next: u64,
    /// A request that was started and whose `done` was never read, with its events.
    in_flight: Option<(String, mpsc::UnboundedReceiver<Value>)>,
    /// The memory directories this kernel was last pointed at.
    harness: Option<KernelContext>,
    /// The images the last cell loaded.
    images: Vec<Value>,
    /// Where this kernel's namespace is snapshotted.
    snapshot: Option<SnapshotTarget>,
    /// The saved namespace could not be revived, so the snapshot on disk is fresher
    /// than this namespace and the final snapshot must not overwrite it.
    pending_restore: bool,
    journal: Option<PathBuf>,
    stderr_task: Option<JoinHandle<()>>,
}

/// One request's events up to its `done`.
struct Finished {
    text: CellText,
    done: Value,
}

impl Kernel {
    /// A kernel over a protocol stream, its pump running.
    fn attach(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        snapshot: Option<SnapshotTarget>,
    ) -> (Self, oneshot::Receiver<Value>) {
        let link = Arc::new(Link::new(Box::new(writer)));
        let (ready_sender, ready) = oneshot::channel();
        tokio::spawn(pump(Arc::clone(&link), reader, ready_sender));
        (
            Self {
                child: None,
                pid: None,
                link,
                next: 0,
                in_flight: None,
                harness: None,
                images: Vec::new(),
                snapshot,
                pending_restore: false,
                journal: None,
                stderr_task: None,
            },
            ready,
        )
    }

    async fn start(
        python: &Path,
        runtime: &Path,
        workspace: &Path,
        data_dir: &Path,
        shell: Option<&Path>,
        snapshot: Option<SnapshotTarget>,
        journal_path: PathBuf,
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
            .stderr(Stdio::piped())
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
        let stderr_pipe = child.stderr.take();
        let pid = child.id();
        let (mut kernel, ready) = Self::attach(stdout, stdin, snapshot);
        kernel.child = Some(child);
        kernel.pid = pid;
        kernel.journal = Some(journal_path.clone());
        if let Some(stderr_pipe) = stderr_pipe {
            let log_path = kernel.snapshot.as_ref().map_or_else(
                || data_dir.join("repl").join(stderr::LOG_FILE),
                |target| target.dir.join(stderr::LOG_FILE),
            );
            let log = match stderr::Log::open(&log_path) {
                Ok((log, note)) => {
                    if let Some(note) = note {
                        kernel.link.diagnostic(&note);
                    }
                    Some(log)
                }
                Err(error) => {
                    kernel.link.diagnostic(&error);
                    None
                }
            };
            let link = Arc::clone(&kernel.link);
            kernel.stderr_task = Some(tokio::spawn(async move {
                stderr::drain(stderr_pipe, log, |text| link.state().stderr.push(text)).await;
            }));
        }
        if let Some(pid) = pid {
            // The identity query can take a moment on Windows; the kernel does not wait.
            let owner = std::process::id();
            tokio::spawn(async move {
                let start_id = journal::process_start_id(pid).await;
                journal::record(&journal_path, owner, pid, true, start_id.as_deref());
            });
        }
        let ready = match tokio::time::timeout(READY_TIMEOUT, ready).await {
            Err(_) => {
                return Err(format!(
                    "the Python kernel did not become ready within {}s. stderr tail:\n{}",
                    READY_TIMEOUT.as_secs(),
                    kernel.stderr_quote().await
                ));
            }
            Ok(Err(_)) => {
                return Err(format!(
                    "the Python kernel exited before it was ready. stderr:\n{}",
                    kernel.stderr_quote().await
                ));
            }
            Ok(Ok(ready)) => ready,
        };
        if ready.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL_VERSION) {
            return Err(format!(
                "the Python kernel spoke an unknown protocol: {ready}"
            ));
        }
        Ok(kernel)
    }

    /// The end of the kernel's stderr, once a dead kernel's last words are in.
    async fn stderr_quote(&mut self) -> String {
        if !self.link.is_open()
            && let Some(task) = self.stderr_task.take()
        {
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        let tail = self.link.state().stderr.last(STDERR_QUOTE_CHARS).to_owned();
        if tail.is_empty() {
            "(empty)".to_owned()
        } else {
            tail
        }
    }

    /// Prepare a new kernel: revive the conversation's snapshot when there is one,
    /// then run the runtime bootstrap, which sets the live handles (`rlm`, `bash`)
    /// over anything restored, as prime-agent orders the two.
    async fn provision(&mut self) -> Result<Revival, String> {
        let existed = self
            .snapshot
            .as_ref()
            .is_some_and(|target| target.payload().is_file());
        let restore = if existed {
            self.restore()
                .await?
                .map_or(Revival::Failed, Revival::Restored)
        } else {
            Revival::NoSnapshot
        };
        let (boot, _) = self
            .execute(BOOTSTRAP_CODE, READY_TIMEOUT, false)
            .await
            .map_err(|error| format!("the Python kernel failed to start: {error}"))?;
        if boot.contains("Traceback") {
            return Err(format!("the REPL runtime failed to load: {boot}"));
        }
        Ok(restore)
    }

    /// The protocol's `restore`. `Ok(None)` when the snapshot could not be revived;
    /// `Err` when the kernel is unusable.
    async fn restore(&mut self) -> Result<Option<RestoreResult>, String> {
        let Some(target) = self.snapshot.clone() else {
            return Ok(None);
        };
        let finished = self
            .run(
                json!({"type": "restore", "path": target.payload().display().to_string()}),
                RESTORE_EXECUTION_TIMEOUT,
            )
            .await?;
        if finished.done["status"] != "ok" {
            self.link.diagnostic(&format!(
                "state restore failed: {}",
                finished.done["reason"].as_str().unwrap_or("failed")
            ));
            self.pending_restore = true;
            return Ok(None);
        }
        self.pending_restore = false;
        Ok(Some(RestoreResult::from_done(&finished.done)))
    }

    /// The protocol's `snapshot`, best-effort and per variable. `Ok(None)` when there
    /// is nowhere to write it or it failed; `Err` when the kernel is unusable.
    async fn snapshot(&mut self, prune: bool) -> Result<Option<SnapshotResult>, String> {
        let Some(target) = self.snapshot.clone() else {
            return Ok(None);
        };
        let finished = self
            .run(target.request(prune), SNAPSHOT_EXECUTION_TIMEOUT)
            .await?;
        if finished.done["status"] != "ok" {
            self.link.diagnostic(&format!(
                "state snapshot failed: {}",
                finished.done["reason"].as_str().unwrap_or("failed")
            ));
            return Ok(None);
        }
        let result = SnapshotResult::from_done(&finished.done);
        // A variable that cannot be serialized (an open file, a socket) is left out,
        // not fatal; what was left out is noted with the kernel's diagnostics.
        if !result.skipped.is_empty() {
            self.link.diagnostic(&format!(
                "state snapshot skipped: {}",
                result
                    .skipped
                    .iter()
                    .map(|(name, reason)| format!("{name} ({reason})"))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
        Ok(Some(result))
    }

    /// Run one cell. `Ok` carries the cell's text and whether it finished without an
    /// error; `Err` means the kernel is unusable and carries what to tell the model.
    /// `background` hands the cell the unattributed output gathered since the last.
    async fn execute(
        &mut self,
        code: &str,
        timeout: Duration,
        background: bool,
    ) -> Result<(String, bool), String> {
        let Finished { mut text, done } = self
            .run(json!({"type": "execute", "code": code}), timeout)
            .await?;
        if background {
            text.background = self.link.take_background();
        }
        self.images = std::mem::take(&mut text.images);
        let worked = done["status"] == "ok" && text.error.is_empty();
        Ok((text.render(), worked))
    }

    /// Send one request and read its events until `done`. A request that outlives
    /// `timeout` is interrupted and given a moment; one that still does not stop
    /// leaves the kernel unusable.
    async fn run(&mut self, mut request: Value, timeout: Duration) -> Result<Finished, String> {
        self.settle_stale().await?;
        self.next += 1;
        let id = format!("cell-{}", self.next);
        request["id"] = Value::String(id.clone());
        let events = self.link.route(&id)?;
        self.link.state().active = Some(id.clone());
        self.in_flight = Some((id.clone(), events));
        self.link.write(&request).await?;
        let mut text = CellText::default();
        if let Some(done) = self.drain(Instant::now() + timeout, &mut text).await? {
            self.settled();
            return Ok(Finished { text, done });
        }
        // Out of time: interrupt, as Ctrl-C does in prime-agent, and give it a moment.
        self.link
            .write(&json!({"type": "interrupt", "id": id}))
            .await?;
        text.note(&format!(
            "[The cell ran longer than {}s and was interrupted.]",
            timeout.as_secs()
        ));
        if let Some(done) = self
            .drain(Instant::now() + INTERRUPT_GRACE, &mut text)
            .await?
        {
            self.settled();
            return Ok(Finished { text, done });
        }
        text.note(&format!(
            "{KERNEL_RESTART_NOTICE} It did not stop after the interrupt."
        ));
        Err(text.render())
    }

    /// A canceled turn left a request running; stop it before the next one starts.
    async fn settle_stale(&mut self) -> Result<(), String> {
        let Some((stale, _)) = &self.in_flight else {
            return Ok(());
        };
        let stale = stale.clone();
        self.link
            .write(&json!({"type": "interrupt", "id": stale}))
            .await?;
        let mut ignored = CellText::default();
        if self
            .drain(Instant::now() + INTERRUPT_GRACE, &mut ignored)
            .await?
            .is_some()
        {
            self.settled();
            return Ok(());
        }
        Err("the previous Python cell could not be stopped; the kernel was restarted".to_owned())
    }

    fn settled(&mut self) {
        self.in_flight = None;
        self.link.state().active = None;
    }

    /// Read the in-flight request's events until its `done` (`Some`) or `deadline`
    /// passes (`None`).
    async fn drain(
        &mut self,
        deadline: Instant,
        text: &mut CellText,
    ) -> Result<Option<Value>, String> {
        loop {
            let Some((_, events)) = self.in_flight.as_mut() else {
                return Ok(None);
            };
            let event = match tokio::time::timeout_at(deadline, events.recv()).await {
                Err(_) => return Ok(None),
                Ok(None) => {
                    let note = self.exit_note().await;
                    text.note(&note);
                    return Err(text.render());
                }
                Ok(Some(event)) => event,
            };
            match event.get("event").and_then(Value::as_str).unwrap_or("") {
                "stdout" => text
                    .stdout
                    .push_str(event["text"].as_str().unwrap_or_default()),
                "stderr" => text
                    .stderr
                    .push_str(event["text"].as_str().unwrap_or_default()),
                // prime-agent's attachment display: an image the model is to see.
                "display" => {
                    if let Some(attachment) = event["data"][ATTACHMENT_DISPLAY_MIME].as_object() {
                        text.images.push(Value::Object(attachment.clone()));
                    }
                }
                "result" => {
                    event["text"]
                        .as_str()
                        .unwrap_or_default()
                        .clone_into(&mut text.result);
                }
                "error" => {
                    let traceback = event
                        .get("traceback")
                        .and_then(Value::as_array)
                        .map(|lines| lines.iter().filter_map(Value::as_str).collect::<String>())
                        .unwrap_or_default();
                    text.error = if traceback.is_empty() {
                        format!(
                            "{}: {}",
                            event["ename"].as_str().unwrap_or("Error"),
                            event["evalue"].as_str().unwrap_or_default()
                        )
                    } else {
                        traceback
                    };
                }
                "done" => return Ok(Some(event)),
                _ => {}
            }
        }
    }

    /// Why the stream ended, with the end of the kernel's stderr when it has any.
    async fn exit_note(&mut self) -> String {
        let reason = self
            .link
            .state()
            .closed
            .clone()
            .unwrap_or_else(|| "The Python kernel exited.".to_owned());
        let tail = self.stderr_quote().await;
        if tail == "(empty)" {
            format!("[{reason}]")
        } else {
            format!("[{reason}]\n[kernel stderr]\n{tail}")
        }
    }

    /// prime-agent's `shutdown`: the final snapshot first - unless the saved namespace
    /// was never revived, when the file on disk is the fresher copy - then host
    /// requests in flight get a moment, then the protocol's `shutdown`, so the runtime
    /// closes its MCP servers and kills the `bash()` process groups a bare kill would
    /// leak, and the process gets a bounded wait before it is killed.
    async fn shutdown(mut self, snapshot: bool) {
        if snapshot && !self.pending_restore && self.link.is_open() {
            let _ = self.snapshot(false).await;
        }
        let mut tasks = self.link.host_tasks.subscribe();
        if tokio::time::timeout(
            HOST_REQUEST_SHUTDOWN_TIMEOUT,
            tasks.wait_for(|count| *count == 0),
        )
        .await
        .is_err()
        {
            self.link.diagnostic(&format!(
                "timed out waiting {}ms for host request task(s) during shutdown",
                HOST_REQUEST_SHUTDOWN_TIMEOUT.as_millis()
            ));
        }
        if self.link.is_open() {
            self.next += 1;
            let id = format!("shutdown-{}", self.next);
            let deadline = Instant::now() + KERNEL_SHUTDOWN_TIMEOUT;
            if let Ok(mut done) = self.link.route(&id)
                && self
                    .link
                    .write(&json!({"type": "shutdown", "id": id}))
                    .await
                    .is_ok()
            {
                // `done`, or the stream ending as the process exits.
                let replied = tokio::time::timeout_at(deadline, done.recv()).await.is_ok();
                let exited = match self.child.as_mut() {
                    Some(child) => tokio::time::timeout_at(deadline, child.wait())
                        .await
                        .is_ok(),
                    None => true,
                };
                if !replied || !exited {
                    self.link.diagnostic(&format!(
                        "graceful shutdown failed (killing instead): the kernel did not shut down within {}ms",
                        KERNEL_SHUTDOWN_TIMEOUT.as_millis()
                    ));
                }
            }
        }
        self.cleanup();
    }

    /// Kill the process if it still runs, and journal that it is gone.
    fn cleanup(&mut self) {
        self.link
            .close("The Python kernel was shut down.".to_owned());
        let Some(mut child) = self.child.take() else {
            return;
        };
        let exited = matches!(child.try_wait(), Ok(Some(_)));
        // Inactive only when the pid provably named our child: it exited, or the kill
        // reached it.
        let stopped = exited || child.start_kill().is_ok();
        if stopped && let (Some(pid), Some(journal_path)) = (self.pid, self.journal.as_ref()) {
            journal::record(journal_path, std::process::id(), pid, false, None);
        }
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        self.cleanup();
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
    /// Images the cell loaded for the model (`attach_image`).
    images: Vec<Value>,
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

/// The kernel venv, as prime-agent's `kernel/bootstrap.ts` builds it - with `uv`
/// first, and, where `uv` is missing or fails, with the system Python's own `venv`
/// and `pip`, so the kernel still gets its packages.
mod venv {
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::time::Duration;

    use tokio::process::Command;

    /// The Python the venv is built on (prime-agent's `PYTHON_VERSION`).
    const PYTHON_VERSION: &str = "3.11";
    /// `dill` for state snapshots, prime-agent's `DEFAULT_RLM_EXTRA_PACKAGES`, and
    /// the packages the bundled skills import (`pillow` for `attach_image`).
    pub const PACKAGES: [&str; 15] = [
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
        // prime-agent-runtime's own dependency, for `rlm.mcp`.
        "mcp>=2,<3",
    ];
    const MARKER: &str = "ha-kernel.json";
    const STEP_TIMEOUT: Duration = Duration::from_mins(15);
    pub const UV_INSTALL_HINT: &str = "https://docs.astral.sh/uv/getting-started/installation/";

    /// What builds the venv, in the order they are tried.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum Builder {
        /// `uv`: Python 3.11 installed by `uv`, as prime-agent builds it.
        Uv(PathBuf),
        /// The system Python (3.11 or newer): `python -m venv`, then `pip install`.
        Pip(PathBuf),
    }

    /// `uv` first, the system Python's `venv` and `pip` after it.
    #[must_use]
    pub fn builders(uv: Option<PathBuf>, system: Option<PathBuf>) -> Vec<Builder> {
        uv.map(Builder::Uv)
            .into_iter()
            .chain(system.map(Builder::Pip))
            .collect()
    }

    /// What the model is told about a venv built by `builder`, after the earlier
    /// builders' `failures`.
    #[must_use]
    pub fn built_notice(builder: &Builder, uv_found: bool, failures: &[String]) -> String {
        match builder {
            Builder::Uv(_) => "[The kernel venv was set up with uv (one-time).]".to_owned(),
            Builder::Pip(_) if !uv_found => "[The kernel venv was set up with the system Python's venv and pip (one-time), because uv is not installed.]".to_owned(),
            Builder::Pip(_) => format!(
                "[The kernel venv was set up with the system Python's venv and pip (one-time), because uv could not build it ({}).]",
                failures.join("; ")
            ),
        }
    }

    /// What the model is told when no builder could make the venv and the kernel
    /// runs on the bare system Python.
    #[must_use]
    pub fn fallback_notice(uv_found: bool, failures: &[String]) -> String {
        let install = if uv_found {
            String::new()
        } else {
            format!(" Install uv ({UV_INSTALL_HINT}) and restart to get it.")
        };
        if failures.is_empty() {
            format!(
                "[The kernel runs on the system Python: uv is not installed, so the kernel venv with prime-agent's default packages was not built.{install}]"
            )
        } else {
            format!(
                "[The kernel venv with prime-agent's default packages could not be built ({}); the kernel runs on the system Python.{install}]",
                failures.join("; ")
            )
        }
    }

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

    /// What the marker records for a `uv` build: a venv built for another list is
    /// rebuilt.
    #[must_use]
    pub fn identity() -> String {
        format!("python {PYTHON_VERSION}; {}", PACKAGES.join(" "))
    }

    /// What the marker records for a build on the system Python.
    #[must_use]
    pub fn system_identity() -> String {
        format!("system python; {}", PACKAGES.join(" "))
    }

    fn identity_of(builder: &Builder) -> String {
        match builder {
            Builder::Uv(_) => identity(),
            Builder::Pip(_) => system_identity(),
        }
    }

    /// Whether the venv exists and was built, by either builder, for the current
    /// package list.
    #[must_use]
    pub fn ready(venv: &Path) -> bool {
        python(venv).is_file()
            && std::fs::read_to_string(venv.join(MARKER))
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|marker| marker["identity"].as_str().map(str::to_owned))
                .is_some_and(|built| built == identity() || built == system_identity())
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

    /// Where `uv` is looked for, in order: `HA_UV` as given, the `uv` shipped beside
    /// the `ha` executable, `uv` on `PATH`, and where uv's own installer puts it.
    #[must_use]
    pub fn uv_candidates(
        explicit: Option<PathBuf>,
        exe_dir: Option<&Path>,
        home: Option<&Path>,
    ) -> Vec<PathBuf> {
        let name = if cfg!(windows) { "uv.exe" } else { "uv" };
        explicit
            .into_iter()
            .chain(exe_dir.map(|dir| dir.join(name)))
            .chain(std::iter::once(PathBuf::from("uv")))
            .chain(home.map(|home| home.join(".local").join("bin").join(name)))
            .collect()
    }

    /// The first working `uv` of [`uv_candidates`] (prime-agent's `ensureUv`).
    /// Nothing is installed: an absent `uv` is reported, not fetched.
    pub async fn find_uv(explicit: Option<PathBuf>) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf));
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
        for candidate in uv_candidates(explicit, exe_dir.as_deref(), home.as_deref()) {
            // A relative candidate other than the bare name would resolve against the
            // working directory, which a repository controls.
            if candidate.components().count() > 1 && !candidate.is_absolute() {
                continue;
            }
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

    /// Run one step; `label` names it in the error, with the last lines it printed.
    async fn run(program: &Path, label: &str, args: &[&str]) -> Result<(), String> {
        let output = tokio::time::timeout(STEP_TIMEOUT, command(program).args(args).output())
            .await
            .map_err(|_| format!("{label} timed out"))?
            .map_err(|error| format!("{label} could not run: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr.lines().rev().take(3).collect::<Vec<_>>();
        Err(format!(
            "{label} failed: {}",
            tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
        ))
    }

    /// Build (or rebuild) the venv with `builder`: the interpreter and the venv, then
    /// every package, and the marker last, so a build cut short is rebuilt next time.
    pub async fn build(builder: &Builder, venv: &Path) -> Result<(), String> {
        if venv.exists() {
            std::fs::remove_dir_all(venv)
                .map_err(|error| format!("the old venv could not be removed: {error}"))?;
        }
        if let Some(parent) = venv.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let venv_text = venv.display().to_string();
        let python_text = python(venv).display().to_string();
        match builder {
            Builder::Uv(uv) => {
                run(uv, "uv python", &["python", "install", PYTHON_VERSION]).await?;
                run(
                    uv,
                    "uv venv",
                    &["venv", &venv_text, "--python", PYTHON_VERSION],
                )
                .await?;
                let mut install = vec!["pip", "install", "--python", python_text.as_str()];
                install.extend(PACKAGES);
                run(uv, "uv pip", &install).await?;
            }
            Builder::Pip(system) => {
                run(system, "python -m venv", &["-m", "venv", &venv_text]).await?;
                let mut install = vec!["-m", "pip", "install", "--disable-pip-version-check"];
                install.extend(PACKAGES);
                run(&python(venv), "pip install", &install).await?;
            }
        }
        std::fs::write(
            venv.join(MARKER),
            serde_json::json!({ "identity": identity_of(builder) }).to_string(),
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
            let CellOutput { text, images } = self
                .shared
                .execute(
                    code,
                    Duration::from_millis(CELL_TIMEOUT_MS),
                    Arc::clone(&self.host),
                    Some(&self.harness),
                )
                .await?;
            Ok(ToolOutput::ExternalTool {
                plugin_id: "repl".to_owned(),
                tool_name: "ipython".to_owned(),
                payload: {
                    let mut payload = json!({ "text": if text.is_empty() { "(no output)".to_owned() } else { text } });
                    if !images.is_empty() {
                        payload["images"] = Value::Array(images);
                    }
                    payload
                },
                inflight: 1,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CellText, Frame, HostReply, HostRequests, Kernel, KernelContext, LinkState,
        MAX_BACKGROUND_OUTPUT_CHARS, MAX_HANDLED_HOST_REQUEST_IDS, NoHostRequests, ReplShared,
        RestoreResult, Revival, SnapshotTarget, read_frame, write_runtime,
    };
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{
        AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream, ReadHalf, WriteHalf,
    };

    #[test]
    fn cell_text_keeps_prime_agents_order() {
        let text = CellText {
            stdout: "out\n".to_owned(),
            stderr: "warn".to_owned(),
            result: "42".to_owned(),
            error: String::new(),
            background: "late".to_owned(),
            notes: vec!["[note]".to_owned()],
            images: Vec::new(),
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
        std::fs::write(
            venv.join("ha-kernel.json"),
            serde_json::json!({ "identity": super::venv::system_identity() }).to_string(),
        )
        .expect("marker");
        assert!(
            super::venv::ready(&venv),
            "a venv the system Python built for the same packages is kept"
        );
    }

    /// `uv` builds the venv first; without it (or when it fails) the system Python's
    /// `venv` and `pip` do; with neither the kernel runs on the bare system Python.
    #[test]
    fn the_venv_is_built_with_uv_first_then_the_system_python() {
        use super::venv::{Builder, builders, built_notice, fallback_notice};
        let uv = std::path::PathBuf::from("uv");
        let system = std::path::PathBuf::from("python");
        assert_eq!(
            builders(Some(uv.clone()), Some(system.clone())),
            vec![Builder::Uv(uv.clone()), Builder::Pip(system.clone())]
        );
        assert_eq!(
            builders(None, Some(system.clone())),
            vec![Builder::Pip(system.clone())]
        );
        assert_eq!(builders(Some(uv.clone()), None), vec![Builder::Uv(uv)]);
        assert!(builders(None, None).is_empty());
        let name = if cfg!(windows) { "uv.exe" } else { "uv" };
        let exe_dir = std::path::Path::new("/opt/ha");
        let home = std::path::Path::new("/home/me");
        assert_eq!(
            super::venv::uv_candidates(
                Some(std::path::PathBuf::from("/tools/uv")),
                Some(exe_dir),
                Some(home)
            ),
            vec![
                std::path::PathBuf::from("/tools/uv"),
                exe_dir.join(name),
                std::path::PathBuf::from("uv"),
                home.join(".local").join("bin").join(name),
            ],
            "HA_UV, then beside ha, then PATH, then uv's installer location"
        );
        assert!(
            built_notice(&Builder::Pip(system.clone()), false, &[]).contains("uv is not installed")
        );
        assert!(
            built_notice(
                &Builder::Pip(system),
                true,
                &["uv venv failed: x".to_owned()]
            )
            .contains("because uv could not build it (uv venv failed: x)")
        );
        let failed = fallback_notice(false, &["pip install failed: no wheel".to_owned()]);
        assert!(
            failed.contains("could not be built (pip install failed: no wheel)")
                && failed.contains("Install uv"),
            "{failed}"
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
    /// a cell past its time is interrupted without losing the kernel, and the
    /// namespace outlives the kernel through its snapshot.
    #[tokio::test]
    #[allow(clippy::too_many_lines, reason = "one kernel, exercised in order")]
    async fn the_kernel_keeps_state_and_answers_host_requests() {
        let directory = tempfile::tempdir().expect("temporary directory");
        // The interpreter found on this machine, as `HA_PYTHON`: a test never builds
        // the kernel venv.
        let Some(python) = super::find_python(None).await else {
            eprintln!("skipped: no Python 3.11+ on this machine");
            return;
        };
        let shared = Arc::new(ReplShared::new(
            directory.path(),
            directory.path(),
            Some(python.display().to_string()),
            super::default_shell(),
        ));
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
        let dirs = KernelContext {
            global: directory.path().join("global-harness"),
            local: directory.path().join("local-harness"),
            skills: super::python_skill_packages(&skills_root.join("SKILL.md")),
            kernel_dir: directory.path().join("kernel-a"),
        };
        assert_eq!(dirs.skills.len(), 2);
        let run = |code: &'static str, seconds: u64, context: KernelContext| {
            let shared = Arc::clone(&shared);
            async move {
                shared
                    .execute(
                        code,
                        Duration::from_secs(seconds),
                        Arc::new(NoHostRequests),
                        Some(&context),
                    )
                    .await
                    .expect("cell")
                    .text
            }
        };
        let first = run("x = 41\nprint('hi')\nx + 1", 60, dirs.clone()).await;
        assert!(
            first.contains("[Python skills unavailable in this kernel: broken_skill (")
                && first.ends_with("hi\n42"),
            "an unavailable skill is reported once, before the first output: {first}"
        );
        assert_eq!(
            run("await hello_skill('ha')", 60, dirs.clone()).await,
            "'hello ha'",
            "a skill module with run() is callable"
        );
        assert_eq!(run("x", 60, dirs.clone()).await, "41", "state persists");
        let spawn = run("await rlm.spawn('task', name='w')", 60, dirs.clone()).await;
        assert!(
            spawn.contains("rlm.run is not available in ha"),
            "a host request is answered: {spawn}"
        );
        let slow = run("while True: await asyncio.sleep(0.05)", 2, dirs.clone()).await;
        assert!(slow.contains("interrupted"), "{slow}");
        assert_eq!(
            run("x", 60, dirs.clone()).await,
            "41",
            "the kernel survived the interrupt"
        );
        // An image the cell loads for the model (prime-agent's attachment display, as
        // `attach_image` emits it) comes back beside the text.
        let loaded = shared
            .execute(
                "from rlm import emit\nemit({'application/vnd.prime-agent.attachment+json': {'mime_type': 'image/png', 'data': 'iVBORw0KGgo=', 'path': 'shot.png'}, 'text/plain': 'loaded'})\n'done'",
                Duration::from_mins(1),
                Arc::new(NoHostRequests),
                Some(&dirs),
            )
            .await
            .expect("cell");
        assert_eq!(loaded.text, "'done'");
        assert_eq!(loaded.images.len(), 1);
        assert_eq!(loaded.images[0]["mime_type"], "image/png");
        // prime-agent's `mcp` object is pre-imported and asks the host for the user's
        // connections.
        let mut servers = std::collections::BTreeMap::new();
        servers.insert(
            "files".to_owned(),
            harness_types::McpServerConfigV2 {
                command: Some("node".to_owned()),
                ..harness_types::McpServerConfigV2::default()
            },
        );
        let mcp_host = Arc::new(crate::interactive::skill_requests::McpRequests::new(
            servers,
            directory.path().to_path_buf(),
        ));
        let connections = shared
            .execute(
                "[c['connectionId'] for c in await mcp.list_connections()]",
                Duration::from_mins(1),
                mcp_host,
                Some(&dirs),
            )
            .await
            .expect("cell");
        assert_eq!(connections.text, "['files']");
        // Memory written through `rlm.harness` lands in the conversation's files, where
        // the host reads its digest from.
        let created = run(
            "rlm.harness.create_memory('Indentation', 'The user prefers tabs').id",
            60,
            dirs.clone(),
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
            let bash = run(
                "(await bash('echo from-bash')).output.strip()",
                60,
                dirs.clone(),
            )
            .await;
            assert_eq!(bash, "'from-bash'");
        }
        // The namespace outlives the kernel: disposing it takes the final snapshot,
        // and the next kernel for the conversation revives it and says so. Snapshots
        // need `dill`, which the kernel venv has and a bare system Python may not.
        let has_dill = run(
            "import importlib.util as _u\n_u.find_spec('dill') is not None",
            60,
            dirs.clone(),
        )
        .await;
        run("kept = [1, 2, 3]", 60, dirs.clone()).await;
        shared.dispose().await;
        assert!(dirs.kernel_dir.join("kernel-stderr.log").is_file());
        if has_dill != "True" {
            let fresh = run("'kept' in globals()", 60, dirs.clone()).await;
            assert!(
                fresh.ends_with("False"),
                "without dill nothing is revived: {fresh}"
            );
            shared.dispose().await;
            eprintln!("snapshot round trip skipped: dill is not installed");
            return;
        }
        assert!(dirs.kernel_dir.join("kernel-state.dill").is_file());
        assert!(dirs.kernel_dir.join("kernel-stderr.log").is_file());
        let revived = run("kept", 60, dirs.clone()).await;
        assert!(
            revived.contains("[python-state-restored]")
                && revived.contains("kept")
                && revived.ends_with("[1, 2, 3]"),
            "{revived}"
        );
        // Another conversation gets its own kernel, with none of this one's state.
        let other = KernelContext {
            kernel_dir: directory.path().join("kernel-b"),
            ..dirs.clone()
        };
        let elsewhere = run("'kept' in globals()", 60, other).await;
        assert!(elsewhere.ends_with("False"), "{elsewhere}");
        shared.dispose().await;
        assert!(
            directory
                .path()
                .join("kernel-b")
                .join("kernel-state.dill")
                .is_file(),
            "the other conversation's kernel was snapshotted when disposed"
        );
    }

    /// The runtime's end of a kernel with no process behind it: the test reads the
    /// host's requests and writes the events.
    struct FakeRuntime {
        requests: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
        events: WriteHalf<DuplexStream>,
    }

    impl FakeRuntime {
        async fn request(&mut self) -> Value {
            let line = tokio::time::timeout(Duration::from_secs(10), self.requests.next_line())
                .await
                .expect("a request in time")
                .expect("read")
                .expect("a line");
            serde_json::from_str(&line).expect("a JSON request")
        }

        async fn emit(&mut self, event: Value) {
            self.raw(&event.to_string()).await;
        }

        async fn raw(&mut self, line: &str) {
            self.events
                .write_all(format!("{line}\n").as_bytes())
                .await
                .expect("write");
            self.events.flush().await.expect("flush");
        }

        async fn quiet_for(&mut self, wait: Duration) -> bool {
            tokio::time::timeout(wait, self.requests.next_line())
                .await
                .is_err()
        }

        async fn done(&mut self, request: &Value) {
            self.emit(json!({"event": "done", "id": request["id"], "status": "ok"}))
                .await;
        }
    }

    fn fake_kernel(snapshot: Option<SnapshotTarget>) -> (Kernel, FakeRuntime) {
        let (host_end, runtime_end) = tokio::io::duplex(1 << 16);
        let (host_read, host_write) = tokio::io::split(host_end);
        let (runtime_read, runtime_write) = tokio::io::split(runtime_end);
        let (kernel, _ready) = Kernel::attach(host_read, host_write, snapshot);
        (
            kernel,
            FakeRuntime {
                requests: BufReader::new(runtime_read).lines(),
                events: runtime_write,
            },
        )
    }

    /// `slow` is answered only once `fast` was: handled one at a time, it never is.
    #[derive(Default)]
    struct GatedHost {
        gate: tokio::sync::Notify,
    }

    impl HostRequests for GatedHost {
        fn handle<'a>(&'a self, request: &'a Value) -> HostReply<'a> {
            Box::pin(async move {
                match request["type"].as_str()? {
                    "slow" => {
                        self.gate.notified().await;
                        Some(Ok(json!("slow done")))
                    }
                    "fast" => {
                        self.gate.notify_one();
                        Some(Ok(json!("fast done")))
                    }
                    "late" => Some(Ok(json!("late done"))),
                    _ => None,
                }
            })
        }
    }

    #[tokio::test]
    async fn host_requests_are_answered_concurrently_once_and_after_their_cell() {
        let (mut kernel, mut runtime) = fake_kernel(None);
        kernel.link.set_host(Arc::new(GatedHost::default()));
        let script = async {
            let execute = runtime.request().await;
            assert_eq!(execute["type"], "execute");
            runtime
                .emit(json!({"event": "host_request", "id": "h1", "data": {"type": "slow"}}))
                .await;
            runtime
                .emit(json!({"event": "host_request", "id": "h2", "data": {"type": "fast"}}))
                .await;
            let mut replies = Vec::new();
            for _ in 0..2 {
                let reply = runtime.request().await;
                assert_eq!(reply["type"], "host_reply");
                replies.push((reply["id"].to_string(), reply["data"]["result"].clone()));
            }
            replies.sort_by(|left, right| left.0.cmp(&right.0));
            assert_eq!(
                replies,
                vec![
                    ("\"h1\"".to_owned(), json!("slow done")),
                    ("\"h2\"".to_owned(), json!("fast done")),
                ]
            );
            runtime
                .emit(json!({"event": "stdout", "id": execute["id"], "text": "hi"}))
                .await;
            runtime.done(&execute).await;
        };
        let (cell, ()) = tokio::join!(kernel.execute("x", Duration::from_secs(10), true), script);
        assert_eq!(cell.expect("cell"), ("hi".to_owned(), true));

        // After the cell went idle: a detached task's request is still answered, a
        // repeated id is not answered twice, and an unknown type gets an error.
        runtime
            .emit(json!({"event": "host_request", "id": "h1", "data": {"type": "late"}}))
            .await;
        runtime
            .emit(json!({"event": "stdout", "id": null, "text": "from a thread"}))
            .await;
        runtime
            .emit(json!({"event": "host_request", "id": "h3", "data": {"type": "late"}}))
            .await;
        let reply = runtime.request().await;
        assert_eq!(reply["id"], "h3");
        assert_eq!(
            reply["data"],
            json!({"status": "ok", "result": "late done"})
        );
        runtime
            .emit(json!({"event": "host_request", "id": "h4", "data": {"type": "rlm.nope"}}))
            .await;
        let reply = runtime.request().await;
        assert_eq!(reply["id"], "h4");
        assert_eq!(reply["data"]["error"], "rlm.nope is not available in ha");

        // Output nobody owned surfaces with the next cell.
        let script = async {
            let execute = runtime.request().await;
            runtime.done(&execute).await;
        };
        let (cell, ()) = tokio::join!(kernel.execute("y", Duration::from_secs(10), true), script);
        let (text, _) = cell.expect("cell");
        assert_eq!(text, "[background output (unattributed)]\nfrom a thread");
    }

    #[tokio::test]
    async fn a_line_that_is_not_the_protocol_ends_the_kernel_and_is_reported() {
        let (mut kernel, mut runtime) = fake_kernel(None);
        let script = async {
            runtime.request().await;
            runtime.raw("Traceback (most recent call last):").await;
        };
        let (cell, ()) = tokio::join!(kernel.execute("x", Duration::from_secs(10), true), script);
        let error = cell.expect_err("the kernel is unusable");
        assert!(
            error.contains("[Kernel protocol error: unparseable protocol line: Traceback"),
            "{error}"
        );
        assert!(
            error.contains("[kernel] unparseable protocol line"),
            "the diagnostic is in the stderr tail: {error}"
        );
        assert!(!kernel.link.is_open());
        assert!(
            kernel.link.route("next").is_err(),
            "nothing waits on a closed stream"
        );
    }

    #[test]
    fn a_frame_must_be_a_known_event_and_routed_ones_need_an_id() {
        assert_eq!(
            super::invalid_frame_reason(&json!({"event": "stdout", "text": "x"})),
            None
        );
        assert_eq!(
            super::invalid_frame_reason(&json!({"event": "surprise"})).as_deref(),
            Some("unknown protocol event")
        );
        assert_eq!(
            super::invalid_frame_reason(&json!({"event": "done", "id": ""})).as_deref(),
            Some("done frame without id")
        );
        assert_eq!(
            super::invalid_frame_reason(&json!({"event": "host_request"})).as_deref(),
            Some("host_request frame without id")
        );
    }

    #[tokio::test]
    async fn a_protocol_line_is_capped() {
        let mut reader: &[u8] = b"{\"a\":1}\nshort\n0123456789ABCDEF\n";
        let mut line = Vec::new();
        assert_eq!(
            read_frame(&mut reader, &mut line, 10).await.expect("read"),
            Frame::Line
        );
        assert_eq!(line, b"{\"a\":1}");
        line.clear();
        assert_eq!(
            read_frame(&mut reader, &mut line, 10).await.expect("read"),
            Frame::Line
        );
        assert_eq!(line, b"short");
        line.clear();
        assert_eq!(
            read_frame(&mut reader, &mut line, 10).await.expect("read"),
            Frame::TooLong
        );
        let mut reader: &[u8] = b"tail";
        line.clear();
        assert_eq!(
            read_frame(&mut reader, &mut line, 10).await.expect("read"),
            Frame::Line
        );
        assert_eq!(line, b"tail");
        line.clear();
        assert_eq!(
            read_frame(&mut reader, &mut line, 10).await.expect("read"),
            Frame::Eof
        );
    }

    #[test]
    fn background_output_and_seen_host_requests_stay_bounded() {
        let mut state = LinkState::default();
        state.append_background(&"a".repeat(MAX_BACKGROUND_OUTPUT_CHARS - 1));
        state.append_background("bcd");
        let text = state.take_background();
        assert_eq!(
            text,
            format!(
                "{}b\n[... background output truncated at {MAX_BACKGROUND_OUTPUT_CHARS} chars ...]",
                "a".repeat(MAX_BACKGROUND_OUTPUT_CHARS - 1)
            )
        );
        state.append_background("x");
        assert_eq!(state.take_background(), "x", "taking resets the cap");

        assert!(state.remember("first"));
        assert!(!state.remember("first"), "a repeated id is answered once");
        for index in 0..MAX_HANDLED_HOST_REQUEST_IDS {
            assert!(state.remember(&index.to_string()));
        }
        assert_eq!(state.handled_ids.len(), MAX_HANDLED_HOST_REQUEST_IDS);
        assert!(state.remember("first"), "the oldest id was forgotten");
    }

    #[tokio::test]
    async fn a_burst_of_cells_is_snapshotted_once_with_prime_agents_caps() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let mut shared = ReplShared::new(directory.path(), directory.path(), None, None);
        shared.snapshot_debounce = Duration::from_millis(50);
        let shared = Arc::new(shared);
        let target = SnapshotTarget {
            dir: directory.path().join("kernel"),
        };
        let (kernel, mut runtime) = fake_kernel(Some(target.clone()));
        shared.kernel.lock().await.kernel = Some(kernel);
        for _ in 0..3 {
            shared.schedule_snapshot();
        }
        let request = runtime.request().await;
        assert_eq!(request["type"], "snapshot");
        assert_eq!(request["max_bytes"], 256 * 1024 * 1024);
        assert_eq!(request["max_variable_bytes"], 16 * 1024 * 1024);
        assert_eq!(request["prune_oversized"], false);
        assert_eq!(request["path"], target.payload().display().to_string());
        assert_eq!(
            request["manifest_path"],
            target.manifest().display().to_string()
        );
        runtime
            .emit(json!({"event": "done", "id": request["id"], "status": "ok", "saved": ["x"], "skipped": [{"name": "f", "reason": "unpicklable"}], "bytes": 12}))
            .await;
        assert!(
            runtime.quiet_for(Duration::from_millis(300)).await,
            "one snapshot per burst"
        );
        let slot = shared.kernel.lock().await;
        let tail = slot
            .kernel
            .as_ref()
            .expect("kernel")
            .link
            .state()
            .stderr
            .last(usize::MAX)
            .to_owned();
        assert!(
            tail.contains("state snapshot skipped: f (unpicklable)"),
            "{tail}"
        );
    }

    #[tokio::test]
    async fn pruning_reports_the_removed_variables() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let shared = ReplShared::new(directory.path(), directory.path(), None, None);
        let target = SnapshotTarget {
            dir: directory.path().join("kernel"),
        };
        let (kernel, mut runtime) = fake_kernel(Some(target));
        shared.kernel.lock().await.kernel = Some(kernel);
        let script = async {
            let request = runtime.request().await;
            assert_eq!(request["prune_oversized"], true);
            runtime
                .emit(json!({"event": "done", "id": request["id"], "status": "ok", "saved": [], "skipped": [], "pruned": ["frame"], "bytes": 0}))
                .await;
        };
        let (pruned, ()) = tokio::join!(shared.prune_oversized_variables(), script);
        assert_eq!(pruned, Some(vec!["frame".to_owned()]));
    }

    #[tokio::test]
    async fn shutdown_snapshots_waits_for_host_requests_then_stops_the_kernel() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (kernel, mut runtime) = fake_kernel(Some(SnapshotTarget {
            dir: directory.path().join("kernel"),
        }));
        let host = Arc::new(GatedHost::default());
        kernel
            .link
            .set_host(Arc::clone(&host) as Arc<dyn HostRequests>);
        let script = async {
            let snapshot = runtime.request().await;
            assert_eq!(snapshot["type"], "snapshot");
            // A detached task's request is still being answered when the kernel goes.
            runtime
                .emit(json!({"event": "host_request", "id": "h1", "data": {"type": "slow"}}))
                .await;
            runtime.done(&snapshot).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
            host.gate.notify_one();
            let reply = runtime.request().await;
            assert_eq!(
                reply["type"], "host_reply",
                "the reply comes before the shutdown"
            );
            let shutdown = runtime.request().await;
            assert_eq!(shutdown["type"], "shutdown");
            runtime.done(&shutdown).await;
        };
        tokio::join!(kernel.shutdown(true), script);
    }

    /// The kernel outlives a turn; the turn's handlers - which hold its store -
    /// must not, or the next turn cannot open the project (`writer_locked`).
    #[tokio::test]
    async fn a_finished_turn_takes_its_handlers_back_from_the_kernel() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let shared = ReplShared::new(directory.path(), directory.path(), None, None);
        let (kernel, _runtime) = fake_kernel(None);
        let host = Arc::new(GatedHost::default());
        kernel
            .link
            .set_host(Arc::clone(&host) as Arc<dyn HostRequests>);
        shared.set_link(Some(Arc::clone(&kernel.link)));
        assert_eq!(
            Arc::strong_count(&host),
            2,
            "the kernel holds the turn's handlers"
        );
        shared.release_host();
        assert_eq!(Arc::strong_count(&host), 1, "the turn got them back");
    }

    #[tokio::test]
    async fn a_namespace_that_was_never_revived_does_not_overwrite_its_snapshot() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (mut kernel, mut runtime) = fake_kernel(Some(SnapshotTarget {
            dir: directory.path().join("kernel"),
        }));
        kernel.pending_restore = true;
        let script = async {
            let first = runtime.request().await;
            assert_eq!(first["type"], "shutdown");
            runtime.done(&first).await;
        };
        tokio::join!(kernel.shutdown(true), script);
    }

    #[test]
    fn the_model_is_told_what_a_new_kernel_revived() {
        let revived = RestoreResult::from_done(&json!({
            "restored": ["df", "x"],
            "failed": [{"name": "sock", "reason": "TypeError: cannot pickle"}],
        }));
        assert_eq!(
            super::start_notice(false, &Revival::Restored(revived.clone())).as_deref(),
            Some(
                "[python-state-restored] Your Python kernel state was revived from your previous session. These names are available again: df, x. These could not be restored and must be recreated if needed: sock."
            )
        );
        assert!(
            super::start_notice(true, &Revival::Restored(revived))
                .is_some_and(|notice| notice.contains("The Python kernel was restarted"))
        );
        assert!(
            super::start_notice(false, &Revival::Failed)
                .is_some_and(|notice| notice.contains("could not be revived"))
        );
        assert_eq!(
            super::start_notice(true, &Revival::NoSnapshot).as_deref(),
            Some(super::KERNEL_RESTART_NOTICE)
        );
        assert_eq!(super::start_notice(false, &Revival::NoSnapshot), None);
    }
}
