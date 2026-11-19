use std::{
    io,
    path::Path,
    process::{ExitStatus, Stdio},
    sync::{Arc, OnceLock},
    time::Duration,
};

use harness_providers::CancellationToken;
use harness_types::{ErrorCode, HarnessError};
#[cfg(unix)]
use nix::{errno::Errno, sys::signal::killpg, unistd::Pid};
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
#[cfg(windows)]
use process_wrap::tokio::{CreationFlags, JobObject};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
    time::{Instant, sleep},
};

use crate::{
    capture::{
        FinalizedCapture, ProcessSpoolConfig, Redactor, SpoolLimits, SpoolWriter, SpooledStream,
        StreamTail, finalize_capture,
    },
    location,
    secrets::ProcessEnvironment,
};

/// How a spawn the operating system refused begins: `py` or `which` on a host
/// without them. No process existed, so it is reported as a plain failure,
/// never as "side effect may be uncertain".
const NOT_STARTED: &str = "the program did not start:";

/// The Windows priority class of what the agent runs, which every process it
/// starts inherits: below normal, so a build that takes every core (cargo's
/// rustc) leaves the terminal and ha's own threads the processor first. An
/// idle machine still gives it every core. At normal priority a build froze
/// the screen it was started from.
#[cfg(windows)]
pub const AGENT_PROCESS_PRIORITY: u32 =
    windows::Win32::System::Threading::BELOW_NORMAL_PRIORITY_CLASS.0;

/// Whether this error is a spawn the operating system refused.
pub(crate) fn never_started(error: &HarnessError) -> bool {
    error.code() == ErrorCode::InvalidPayload && error.message().starts_with(NOT_STARTED)
}

/// How long a kill may take to be confirmed as a fully reaped tree.
const CLEANUP_BOUND: Duration = Duration::from_secs(5);

/// The only host environment names a tool process inherits.
///
/// A child never inherits the host environment wholesale: an agent host holds
/// provider credentials, and a model that can start a process must not be able
/// to read them by asking. The list is the smallest set that lets a shell, a
/// compiler and a test runner work; anything else a call genuinely needs must
/// be named as a `secret://` reference the operator exposed.
pub const PROCESS_ENVIRONMENT_ALLOWLIST: &[&str] = &[
    // Shell and executable resolution.
    "PATH",
    "PATHEXT",
    "SystemRoot",
    "SystemDrive",
    "windir",
    "COMSPEC",
    "SHELL",
    // Temporary directories.
    "TEMP",
    "TMP",
    "TMPDIR",
    // User context a toolchain expects to exist.
    "HOME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "USER",
    // Locale.
    "LANG",
    "LC_ALL",
    // Rust toolchain locations, so a real build/test runner resolves its own
    // compiler instead of falling back to a guessed home directory.
    "CARGO_HOME",
    "RUSTUP_HOME",
];

/// The host environment a child may inherit from, as a snapshot.
///
/// It exists as a value rather than as a direct `std::env` read so that the M12
/// capability probe can measure the allowlist against a *known* host
/// environment instead of whatever the test process happens to carry. The
/// production snapshot is [`HostEnvironment::from_process`]; nothing else about
/// the spawn path changes, and the allowlist is still applied at spawn time.
#[derive(Clone, Debug, Default)]
pub struct HostEnvironment {
    values: Vec<(String, String)>,
}

impl HostEnvironment {
    /// The real environment of this process.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            values: std::env::vars().collect(),
        }
    }

    #[must_use]
    pub fn from_pairs(values: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    /// This snapshot plus the named values, replacing any value with that name.
    #[must_use]
    pub fn with_values(mut self, extra: impl IntoIterator<Item = (String, String)>) -> Self {
        for (name, value) in extra {
            self.values
                .retain(|(existing, _)| !same_name(existing, &name));
            self.values.push((name, value));
        }
        self
    }

    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(existing, _)| same_name(existing, name))
            .map(|(_, value)| value.as_str())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

/// Whether two environment names are the same variable.
///
/// Windows environment blocks are case-insensitive — the platform itself spells
/// the search path `Path` — so an allowlist entry of `PATH` has to match it.
/// Unix names are case-sensitive and are compared as such.
fn same_name(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        left.eq_ignore_ascii_case(right)
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// How the process tree's cleanup was established.
///
/// This is recorded instead of a bare boolean because "the kill request
/// succeeded" and "every process in the tree is gone" are different claims.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TreeCleanup {
    /// The call was withdrawn before a process existed.
    NothingToClean,
    /// The direct process exited. The backend did not report that every
    /// descendant in its process container is gone.
    ReapedOnExit,
    /// A kill was requested and the backend confirmed the whole tree is gone.
    KilledAndReaped,
}

impl TreeCleanup {
    #[must_use]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NothingToClean => "nothing_to_clean",
            Self::ReapedOnExit => "reaped_on_exit",
            Self::KilledAndReaped => "killed_and_reaped",
        }
    }
}

enum WaitSignal {
    Exited(io::Result<ExitStatus>),
    TimedOut,
    Canceled,
}

fn process_execution_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// How many host-internal processes may run at once.
const INTERNAL_PROCESS_PERMITS: usize = 4;

fn internal_process_permits() -> &'static tokio::sync::Semaphore {
    static PERMITS: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    PERMITS.get_or_init(|| tokio::sync::Semaphore::new(INTERNAL_PROCESS_PERMITS))
}

/// Which queue a process waits in before it may start.
///
/// The model's own processes (`run_shell`, `run_process`, quality gates) share
/// one host-wide permit: that queue is part of the tool contract (a call
/// withdrawn while queued never runs, and the receipt says it queued). The
/// processes the host starts for itself - the git read tools, the
/// destructive-git guard's status probe, hooks - are short, bounded and
/// side-effect free as far as the queue is concerned, and serializing them
/// behind a model's thirty-second build made a parallel batch of git reads run
/// one at a time. They take one of a few permits of their own instead.
///
/// The single permit was never what kept a Job Object safe: every child gets
/// its own job and its own completion port (process-wrap creates both per
/// spawn), so two children never read each other's completion messages, and
/// std serializes the `CreateProcess` calls themselves so pipe handles are not
/// inherited across children. What still must hold - one `wait` per child and
/// no `try_wait` before it - is per child and unchanged.
///
/// `Owned` is the queue of one tool service - one agent's turn: its calls
/// queue as `Exclusive` ones do, and another agent's do not wait on them. The
/// host-wide queue made a parent's 15-second sleep wait out its child's
/// 50-second one, and a conversation's shell every other agent's build in the
/// same worker.
#[derive(Clone, Debug)]
pub(crate) enum ProcessLane {
    Exclusive,
    Internal,
    Owned(ProcessQueue),
}

/// One owner's process queue: the model's processes of one tool service run
/// one at a time, as the tool contract says, beside other owners' processes.
#[derive(Clone, Debug, Default)]
pub struct ProcessQueue(Arc<tokio::sync::Mutex<()>>);

/// The permit a running process holds; released when the run ends.
enum ProcessPermit {
    Exclusive(
        #[allow(dead_code, reason = "held for its drop")] tokio::sync::MutexGuard<'static, ()>,
    ),
    Internal(
        #[allow(dead_code, reason = "held for its drop")] tokio::sync::SemaphorePermit<'static>,
    ),
    Owned(#[allow(dead_code, reason = "held for its drop")] tokio::sync::OwnedMutexGuard<()>),
}

impl ProcessPermit {
    /// The permit without waiting, if one is free.
    fn try_acquire(lane: &ProcessLane) -> Option<Self> {
        match lane {
            ProcessLane::Exclusive => process_execution_lock()
                .try_lock()
                .ok()
                .map(Self::Exclusive),
            ProcessLane::Internal => internal_process_permits()
                .try_acquire()
                .ok()
                .map(Self::Internal),
            ProcessLane::Owned(queue) => {
                Arc::clone(&queue.0).try_lock_owned().ok().map(Self::Owned)
            }
        }
    }

    /// Wait for the permit. `None` only if the semaphore were closed, which
    /// this module never does.
    async fn acquire(lane: &ProcessLane) -> Option<Self> {
        match lane {
            ProcessLane::Exclusive => Some(Self::Exclusive(process_execution_lock().lock().await)),
            ProcessLane::Internal => internal_process_permits()
                .acquire()
                .await
                .ok()
                .map(Self::Internal),
            ProcessLane::Owned(queue) => Some(Self::Owned(Arc::clone(&queue.0).lock_owned().await)),
        }
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub(crate) struct ProcessResult {
    pub executable: String,
    /// Shell selection for `run_shell`; structured process calls leave this absent.
    pub shell: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    /// Whether this call had to wait for its process permit.
    ///
    /// It is recorded so a canceled queue member is distinguishable from a call
    /// that was withdrawn before it ever reached the runner.
    pub queued: bool,
    pub tree_cleanup: TreeCleanup,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// The end of each stream, which is what the model reads of a command.
    pub stdout_tail: StreamTail,
    pub stderr_tail: StreamTail,
    /// The spooled capture, when the call actually ran a process. It is the
    /// durable copy of everything the process wrote, bounded by the quota.
    pub capture: Option<FinalizedCapture>,
}

/// A structured process the model asked for, in `queue` - its tool service's
/// own - instead of the host-wide queue: it waits only for that service's
/// other processes. It starts in `dir`, with the `PATH` an earlier shell call
/// kept when there is one (`location`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_structured_in(
    queue: &ProcessQueue,
    dir: &Path,
    path: Option<&str>,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
) -> Result<ProcessResult, HarnessError> {
    let host = host_with_path(&HostEnvironment::from_process(), path);
    // A program named bare is looked up in the `PATH` an earlier shell call
    // left, as a shell would: not found there is not started.
    if let Some(path) = path
        && !location::program_on_path(executable, path, dir, host.lookup("PATHEXT"))
    {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            format!(
                "{NOT_STARTED} `{executable}`: it is not on the PATH an earlier shell call set; nothing ran"
            ),
        ));
    }
    run(
        dir,
        executable,
        args,
        timeout_ms,
        cancellation,
        environment,
        spool,
        &host,
        None,
        ProcessLane::Owned(queue.clone()),
    )
    .await
}

/// `host` with the `PATH` a shell call kept, when there is one.
fn host_with_path(host: &HostEnvironment, path: Option<&str>) -> HostEnvironment {
    match path {
        Some(path) => host
            .clone()
            .with_values([("PATH".to_owned(), path.to_owned())]),
        None => host.clone(),
    }
}

/// [`run_shell`], in `queue` instead of the host-wide queue. It starts in `dir`
/// with the `PATH` an earlier call kept, and with `track` it also reads where
/// the command left the shell (`location`): what its epilogue wrote, which is
/// nothing when the command exited itself, was killed, timed out or was
/// canceled.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_shell_in(
    queue: &ProcessQueue,
    dir: &Path,
    path: Option<&str>,
    track: bool,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
) -> Result<(ProcessResult, Option<location::End>), HarnessError> {
    // The side file is in a directory of this user alone (`location`), not in
    // the spool other users can reach.
    let side_file = if track { location::side_file() } else { None };
    let result = run_shell_in_lane(
        dir,
        command,
        timeout_ms,
        cancellation,
        environment,
        spool,
        &HostEnvironment::from_process(),
        ProcessLane::Owned(queue.clone()),
        path,
        side_file.as_ref().map(location::SideFile::path),
    )
    .await;
    let end = side_file.and_then(|side_file| side_file.take());
    result.map(|result| (result, end))
}

/// The same run, against an explicit host-environment snapshot.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_structured_with_host(
    root: &Path,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
    host: &HostEnvironment,
) -> Result<ProcessResult, HarnessError> {
    run(
        root,
        executable,
        args,
        timeout_ms,
        cancellation,
        environment,
        spool,
        host,
        None,
        ProcessLane::Exclusive,
    )
    .await
}

/// Run an executable with bounded JSON or other explicit bytes on stdin.
///
/// The input is capped before spawn so the hook channel cannot turn into an
/// unbounded memory or pipe write. Hook processes receive the same scrubbed
/// environment as every other structured process.
pub async fn run_hook_command(
    root: &Path,
    executable: &str,
    args: &[String],
    input: &[u8],
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> Result<HookProcessResult, HarnessError> {
    run_hook_command_with_host(
        root,
        executable,
        args,
        input,
        timeout_ms,
        cancellation,
        &HostEnvironment::from_process(),
    )
    .await
}

/// Same bounded hook run with an explicit host environment for tests.
pub async fn run_hook_command_with_host(
    root: &Path,
    executable: &str,
    args: &[String],
    input: &[u8],
    timeout_ms: u64,
    cancellation: CancellationToken,
    host: &HostEnvironment,
) -> Result<HookProcessResult, HarnessError> {
    if input.len() > 8 * 1024 {
        return Err(HarnessError::new(
            ErrorCode::OutputLimitExceeded,
            "hook JSON input exceeds the 8 KiB limit",
        ));
    }
    let spool = ProcessSpoolConfig::default();
    let result = run(
        root,
        executable,
        args,
        timeout_ms.min(60_000),
        cancellation,
        &ProcessEnvironment::empty(),
        &spool,
        host,
        Some(input.to_vec()),
        ProcessLane::Internal,
    )
    .await?;
    // A hook (a formatter, say) may have rewritten files.
    crate::walk::settle_changes(root).await;
    Ok(HookProcessResult {
        exit_code: result.exit_code,
        status: if result.timed_out {
            HookProcessStatus::TimedOut
        } else if result.canceled {
            HookProcessStatus::Canceled
        } else {
            HookProcessStatus::Exited
        },
        stdout: result.stdout,
        stderr: result.stderr,
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
    })
}

/// Run a shell command the user wrote themselves - an autonomous quality gate -
/// in the workspace, with the same shell and scrubbed environment the model's
/// shell tool gets, bounded by `timeout_ms` and stopped (tree and all) on
/// cancellation.
pub async fn run_user_command(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> Result<HookProcessResult, HarnessError> {
    let result = run_shell(
        root,
        command,
        timeout_ms,
        cancellation,
        &ProcessEnvironment::empty(),
        &ProcessSpoolConfig::default(),
    )
    .await?;
    crate::walk::settle_changes(root).await;
    Ok(HookProcessResult {
        exit_code: result.exit_code,
        status: if result.timed_out {
            HookProcessStatus::TimedOut
        } else if result.canceled {
            HookProcessStatus::Canceled
        } else {
            HookProcessStatus::Exited
        },
        stdout: result.stdout,
        stderr: result.stderr,
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
    })
}

/// Bounded output and status from one hook process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HookProcessResult {
    pub exit_code: Option<i32>,
    pub status: HookProcessStatus,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Completion state of one bounded hook process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HookProcessStatus {
    Exited,
    TimedOut,
    Canceled,
}

pub(crate) async fn run_shell(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
) -> Result<ProcessResult, HarnessError> {
    run_shell_with_host(
        root,
        command,
        timeout_ms,
        cancellation,
        environment,
        spool,
        &HostEnvironment::from_process(),
    )
    .await
}

/// The same explicit shell run, against an explicit host-environment snapshot.
pub(crate) async fn run_shell_with_host(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
    host: &HostEnvironment,
) -> Result<ProcessResult, HarnessError> {
    run_shell_in_lane(
        root,
        command,
        timeout_ms,
        cancellation,
        environment,
        spool,
        host,
        ProcessLane::Exclusive,
        None,
        None,
    )
    .await
}

/// A shell command the host runs for itself (the destructive-git guard's
/// relocated status probe): it does not queue behind the model's processes.
pub(crate) async fn run_internal_shell(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
    spool: &ProcessSpoolConfig,
) -> Result<ProcessResult, HarnessError> {
    run_shell_in_lane(
        root,
        command,
        timeout_ms,
        cancellation,
        &ProcessEnvironment::empty(),
        spool,
        &HostEnvironment::from_process(),
        ProcessLane::Internal,
        None,
        None,
    )
    .await
}

/// A git command the host runs for itself - the git read tools, the guard's
/// status probe, the receipts' commit and diff probes. Same scrubbed
/// environment and bounds as any structured process, but in the internal lane.
pub(crate) async fn run_internal_git(
    root: &Path,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
    spool: &ProcessSpoolConfig,
) -> Result<ProcessResult, HarnessError> {
    run(
        root,
        "git",
        args,
        timeout_ms,
        cancellation,
        &ProcessEnvironment::empty(),
        spool,
        &HostEnvironment::from_process(),
        None,
        ProcessLane::Internal,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_shell_in_lane(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
    host: &HostEnvironment,
    lane: ProcessLane,
    kept_path: Option<&str>,
    side_file: Option<&Path>,
) -> Result<ProcessResult, HarnessError> {
    // With a side file, the command is followed by the epilogue that writes
    // where the shell ended (`location`).
    let command = match side_file {
        Some(side_file) => location::wrap(location::Shell::native(), command, side_file),
        None => command.to_owned(),
    };
    #[cfg(windows)]
    let (executable, shell) = windows_shell(host);
    #[cfg(windows)]
    let args = vec![
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        command,
    ];
    #[cfg(not(windows))]
    let (executable, args) = ("sh".to_owned(), vec!["-c".to_owned(), command]);
    // The shell is found in the host's own `PATH` and started by that path, so
    // the `PATH` an earlier call left changes only what the command runs: a call
    // that emptied it left the next one without a shell to start.
    let name = executable.clone();
    let executable = if kept_path.is_some() {
        location::absolute_program(&executable, host.lookup("PATH"))
            .map_or(executable, |path| path.to_string_lossy().into_owned())
    } else {
        executable
    };
    let host = &host_with_path(host, kept_path);
    let mut result = run(
        root,
        &executable,
        &args,
        timeout_ms,
        cancellation,
        environment,
        spool,
        host,
        None,
        lane,
    )
    .await?;
    // The receipt names the shell, not where it was found.
    result.executable = name;
    #[cfg(windows)]
    {
        result.shell = Some(shell);
    }
    #[cfg(not(windows))]
    {
        result.shell = Some("sh".to_owned());
    }
    Ok(result)
}

#[cfg(windows)]
fn windows_shell(host: &HostEnvironment) -> (String, String) {
    // Probing means a stat per PATH directory - dozens on a developer machine,
    // each one slow under antivirus - and the answer only changes with PATH, so
    // it is remembered per PATH value. A snapshot with another PATH (a test's,
    // say) is probed on its own.
    static FOUND: OnceLock<std::sync::Mutex<std::collections::HashMap<String, bool>>> =
        OnceLock::new();
    let path = host.lookup("PATH").unwrap_or_default();
    let cache = FOUND.get_or_init(Default::default);
    let cached = cache.lock().ok().and_then(|found| found.get(path).copied());
    let has_pwsh = cached.unwrap_or_else(|| {
        let found = path.split(';').any(|directory| {
            let directory = directory.trim_matches('"');
            Path::new(directory).join("pwsh.exe").is_file()
        });
        if let Ok(mut cache) = cache.lock() {
            cache.insert(path.to_owned(), found);
        }
        found
    });
    if has_pwsh {
        ("pwsh".to_owned(), "powershell-7".to_owned())
    } else {
        (
            "powershell.exe".to_owned(),
            "powershell-5.1 (pwsh not found)".to_owned(),
        )
    }
}

/// The runtime settings that keep a Cygwin or MSYS program's arguments as given.
///
/// Started from a native Windows process, such a program expands wildcards in
/// its own command line: Git's `find` called with `-name *.txt` in a directory
/// holding `big.txt` searched for `big.txt` alone, found nothing and exited 0.
/// A structured call's arguments are already words, so nothing may expand them
/// again. A value a grant resolved for the call still wins.
///
/// These are fixed values, never copied from the host, so they reveal nothing
/// the host holds; they are published next to the allowlist for that reason.
pub const PROCESS_ENVIRONMENT_FIXED: &[(&str, &str)] = if cfg!(windows) {
    &[("MSYS", "noglob"), ("CYGWIN", "noglob")]
} else {
    &[]
};

/// The program a bare name starts, found the way a Windows shell finds it.
///
/// `CreateProcess` only tries `<name>.exe`, so the batch shims most JavaScript
/// tools install - `npm.cmd`, `npx.cmd`, `pnpm.cmd`, `yarn.cmd` - were "program
/// not found" for `run_process`. A name without a directory or an extension is
/// looked up in the child's own `PATH`, trying each `PATHEXT` extension in each
/// directory in order. std starts a resolved `.cmd`/`.bat` through `cmd.exe`
/// with its own argument escaping, and refuses arguments it cannot pass safely.
/// Anything else, or a name found nowhere, is left for `CreateProcess` as given.
#[cfg(windows)]
fn windows_program(
    executable: &str,
    environment: &ProcessEnvironment,
    host: &HostEnvironment,
) -> std::path::PathBuf {
    let given = std::path::PathBuf::from(executable);
    if executable.contains(['/', '\\', ':']) || given.extension().is_some() {
        return given;
    }
    let child = |name: &str| {
        environment
            .values()
            .iter()
            .rev()
            .find(|(existing, _)| same_name(existing, name))
            .map(|(_, value)| value.as_str())
            .or_else(|| host.lookup(name))
    };
    let extensions = child("PATHEXT")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(".COM;.EXE;.BAT;.CMD");
    for directory in child("PATH").unwrap_or_default().split(';') {
        let directory = directory.trim().trim_matches('"');
        if directory.is_empty() {
            continue;
        }
        for extension in extensions
            .split(';')
            .filter(|extension| !extension.is_empty())
        {
            let candidate = Path::new(directory).join(format!("{executable}{extension}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    given
}

#[cfg(all(test, windows))]
mod windows_program_tests {
    use super::{HostEnvironment, run_structured_with_host, windows_program};
    use crate::{capture::ProcessSpoolConfig, secrets::ProcessEnvironment};
    use harness_providers::CancellationToken;

    fn host(path: &std::path::Path) -> HostEnvironment {
        HostEnvironment::from_pairs([
            ("PATH".to_owned(), path.display().to_string()),
            (
                "SystemRoot".to_owned(),
                std::env::var("SystemRoot").expect("Windows system root"),
            ),
            (
                "COMSPEC".to_owned(),
                std::env::var("COMSPEC").expect("Windows command processor"),
            ),
        ])
    }

    #[test]
    fn a_bare_name_finds_its_batch_shim_through_path_and_pathext() {
        let bin = tempfile::tempdir().expect("bin");
        std::fs::write(bin.path().join("tool.cmd"), "@echo off").expect("shim");
        let host = host(bin.path());
        let empty = ProcessEnvironment::empty();
        // PATHEXT spells the extension; Windows paths ignore case.
        assert!(
            windows_program("tool", &empty, &host)
                .as_os_str()
                .eq_ignore_ascii_case(bin.path().join("tool.cmd").as_os_str())
        );
        // A path or an extension is the caller's choice; an unknown name is
        // left for CreateProcess to report.
        assert_eq!(
            windows_program("tool.exe", &empty, &host),
            std::path::PathBuf::from("tool.exe")
        );
        assert_eq!(
            windows_program(".\\tool", &empty, &host),
            std::path::PathBuf::from(".\\tool")
        );
        assert_eq!(
            windows_program("missing", &empty, &host),
            std::path::PathBuf::from("missing")
        );
    }

    #[tokio::test]
    async fn run_process_starts_a_cmd_shim_by_its_bare_name() {
        let bin = tempfile::tempdir().expect("bin");
        std::fs::write(
            bin.path().join("shim.cmd"),
            "@echo off\r\necho shim-ran %1\r\n",
        )
        .expect("shim");
        let workspace = tempfile::tempdir().expect("workspace");
        let result = run_structured_with_host(
            workspace.path(),
            "shim",
            &["lint".to_owned()],
            60_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &ProcessSpoolConfig::default(),
            &host(bin.path()),
        )
        .await
        .expect("the shim starts");
        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert!(result.stdout.contains("shim-ran lint"), "{}", result.stdout);
        assert_eq!(result.executable, "shim");
    }

    /// Git's `find -name *.txt` matched only the workspace's own `big.txt`:
    /// the MSYS runtime expanded the pattern before `find` saw it.
    #[tokio::test]
    async fn a_cygwin_or_msys_program_is_told_not_to_expand_its_arguments() {
        let bin = tempfile::tempdir().expect("bin");
        std::fs::write(
            bin.path().join("runtime.cmd"),
            "@echo off\r\necho msys=%MSYS% cygwin=%CYGWIN%\r\n",
        )
        .expect("shim");
        let workspace = tempfile::tempdir().expect("workspace");
        let result = run_structured_with_host(
            workspace.path(),
            "runtime",
            &[],
            60_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &ProcessSpoolConfig::default(),
            &host(bin.path()),
        )
        .await
        .expect("the shim starts");
        assert!(
            result.stdout.contains("msys=noglob cygwin=noglob"),
            "{}",
            result.stdout
        );
        // Where Git for Windows is installed, its own find shows it end to end.
        let find = std::path::Path::new(r"C:\Program Files\Git\usr\bin\find.exe");
        if !find.is_file() {
            return;
        }
        std::fs::create_dir(workspace.path().join("a")).expect("a");
        std::fs::write(workspace.path().join("a").join("1.txt"), "").expect("1.txt");
        std::fs::write(workspace.path().join("big.txt"), "").expect("big.txt");
        let found = run_structured_with_host(
            workspace.path(),
            &find.display().to_string(),
            &["a", "-type", "f", "-name", "*.txt"].map(str::to_owned),
            60_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &ProcessSpoolConfig::default(),
            &host(bin.path()),
        )
        .await
        .expect("find starts");
        assert_eq!(found.exit_code, Some(0), "stderr: {}", found.stderr);
        assert!(found.stdout.contains("1.txt"), "{}", found.stdout);
    }
}

#[cfg(all(test, windows))]
mod windows_shell_tests {
    use super::{HostEnvironment, run_shell_with_host, windows_shell};
    use crate::{capture::ProcessSpoolConfig, secrets::ProcessEnvironment};
    use harness_providers::CancellationToken;

    #[test]
    fn g14_run_shell_falls_back_when_path_has_no_pwsh() {
        let host = HostEnvironment::from_pairs([("PATH".to_owned(), "C:\\missing".to_owned())]);
        assert_eq!(
            windows_shell(&host),
            (
                "powershell.exe".to_owned(),
                "powershell-5.1 (pwsh not found)".to_owned()
            )
        );
    }

    #[tokio::test]
    async fn g14_fallback_receipt_names_powershell_51() {
        let root = std::env::var("SystemRoot").expect("Windows system root");
        let shell_dir = std::path::Path::new(&root).join("System32/WindowsPowerShell/v1.0");
        assert!(shell_dir.join("powershell.exe").is_file());
        let workspace = tempfile::tempdir().expect("workspace");
        let host = HostEnvironment::from_pairs([
            ("PATH".to_owned(), shell_dir.display().to_string()),
            ("SystemRoot".to_owned(), root),
        ]);
        // Measured on the GitHub windows-latest runner (24/09/2026): the first
        // Windows PowerShell 5.1 start there is cold and took the whole 10 s this
        // test used to allow, so the run was killed (TerminateProcess reports exit
        // code 1) and the test failed in every job while passing on a warm desktop.
        // The bound is for a hang, not for start-up time.
        let result = run_shell_with_host(
            workspace.path(),
            "Write-Output 'shell-fallback-ok'",
            60_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &ProcessSpoolConfig::default(),
            &host,
        )
        .await
        .expect("fallback starts");
        assert!(
            !result.timed_out,
            "Windows PowerShell did not finish within the bound; stderr: {}",
            result.stderr
        );
        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert_eq!(
            result.shell.as_deref(),
            Some("powershell-5.1 (pwsh not found)")
        );
        assert!(result.stdout.contains("shell-fallback-ok"));
    }
}

#[cfg(test)]
mod spool_cleanup_tests {
    use super::{HostEnvironment, run_structured_with_host};
    use crate::{
        capture::{ProcessSpoolConfig, SpoolLimits},
        secrets::ProcessEnvironment,
    };
    use harness_providers::CancellationToken;

    /// A git tool reads the previews and drops the result: before the fix every
    /// such run left a `.artifact` copy in the spool directory for good.
    #[tokio::test]
    async fn a_git_run_leaves_nothing_in_the_spool_directory() {
        let workspace = tempfile::tempdir().expect("workspace");
        let spool_root = tempfile::tempdir().expect("spool root");
        let spool = ProcessSpoolConfig::new(spool_root.path(), SpoolLimits::default());
        let result = run_structured_with_host(
            workspace.path(),
            "git",
            &["--version".to_owned()],
            30_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &spool,
            &HostEnvironment::from_process(),
        )
        .await
        .expect("git runs");
        assert_eq!(result.exit_code, Some(0), "stderr: {}", result.stderr);
        assert!(result.stdout.contains("git version"));
        drop(result);
        let left: Vec<_> = std::fs::read_dir(spool_root.path())
            .expect("spool root")
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert!(left.is_empty(), "spool files left behind: {left:?}");
    }
}

/// Where a running tool's process output goes as it is written: pa-agent's
/// `tool_execution_update`, which shows a long command's output while it runs.
/// The turn driver sets it around a tool call; output is redacted first.
pub type ToolProgressSink = std::sync::Arc<dyn Fn(&[u8]) + Send + Sync>;

tokio::task_local! {
    pub static TOOL_PROGRESS: ToolProgressSink;
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)] // one process lifecycle, told in order
async fn run(
    root: &Path,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
    host: &HostEnvironment,
    stdin: Option<Vec<u8>>,
    lane: ProcessLane,
) -> Result<ProcessResult, HarnessError> {
    // A single host-wide runner permit makes the model's concurrent tool calls
    // deterministic; host-internal processes have a few permits of their own
    // (see `ProcessLane`). Neither bypasses the per-process tree ownership or
    // output bounds.
    //
    // The queue is cancellation-aware: a call withdrawn by its caller must not
    // start a process later, when the permit finally reaches it. The fast path
    // keeps the uncontended case free of a select branch.
    let (guard, queued) = if let Some(guard) = ProcessPermit::try_acquire(&lane) {
        (guard, false)
    } else {
        let guard = tokio::select! {
            guard = ProcessPermit::acquire(&lane) => guard,
            () = cancellation.cancelled() => {
                return Ok(canceled_before_spawn(executable, true));
            }
        };
        let Some(guard) = guard else {
            return Err(HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                "the process permit queue was closed",
            ));
        };
        (guard, true)
    };
    let _execution_guard = guard;
    // A call canceled in the instant the permit became available must not start
    // a process either: the caller has already withdrawn it.
    if cancellation.is_cancelled() {
        return Ok(canceled_before_spawn(executable, queued));
    }
    let pipe_stdin = stdin.is_some();
    #[cfg(windows)]
    let program = windows_program(executable, environment, host);
    #[cfg(not(windows))]
    let program = executable;
    let mut command = CommandWrap::with_new(program, |child_command| {
        child_command
            .args(args)
            .current_dir(root)
            .stdin(if pipe_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // The host environment is not inherited: only the allowlist, plus the
        // values this call's grants resolved. A child that dumps its own
        // environment therefore shows the host's credentials only if the
        // operator exposed them for this exact action. The allowlist is applied
        // here, at spawn, to the snapshot the caller passed in.
        child_command.env_clear();
        for name in PROCESS_ENVIRONMENT_ALLOWLIST {
            if let Some(value) = host.lookup(name) {
                child_command.env(name, value);
            }
        }
        for (name, value) in PROCESS_ENVIRONMENT_FIXED {
            child_command.env(name, value);
        }
        for (name, value) in environment.values() {
            child_command.env(name, value);
        }
    });
    command.wrap(KillOnDrop);
    #[cfg(windows)]
    command.wrap(CreationFlags(
        windows::Win32::System::Threading::PROCESS_CREATION_FLAGS(AGENT_PROCESS_PRIORITY),
    ));
    #[cfg(windows)]
    command.wrap(JobObject);
    #[cfg(unix)]
    command.wrap(ProcessSession);
    let mut child = command.spawn().map_err(|error| match error.kind() {
        // The operating system refused to create the process: nothing ran, so
        // the outcome is known and the caller may correct the call and retry.
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("{NOT_STARTED} `{executable}`: {error}; nothing ran"),
            )
        }
        _ => HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!("cannot spawn structured process: {error}"),
        ),
    })?;
    #[cfg(unix)]
    let process_group_id = child.id().ok_or_else(|| {
        HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "spawned process has no process group ID",
        )
    })?;
    if let Some(input) = stdin {
        let mut child_stdin = child.stdin().take().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                "hook process stdin was not available",
            )
        })?;
        child_stdin.write_all(&input).await.map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("hook process did not accept its bounded input: {error}"),
            )
        })?;
        child_stdin.shutdown().await.map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("hook process input could not be closed: {error}"),
            )
        })?;
    }
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    let limits = spool.limits();
    // The readers run on tasks of their own, where the caller's task-local
    // sink is not set: it is taken here and handed to them.
    let progress = TOOL_PROGRESS.try_with(std::sync::Arc::clone).ok();
    // Both streams stream to their own spool file. Memory holds a bounded head
    // preview per stream and nothing else, so a process that logs a gigabyte
    // costs the same as one that logs a line.
    let stdout_reader = stdout.map(|stream| {
        tokio::spawn(read_spooled(
            stream,
            spool.root().to_owned(),
            limits,
            "stdout",
            environment.redactions().to_vec(),
            progress.clone(),
        ))
    });
    let stderr_reader = stderr.map(|stream| {
        tokio::spawn(read_spooled(
            stream,
            spool.root().to_owned(),
            limits,
            "stderr",
            environment.redactions().to_vec(),
            progress.clone(),
        ))
    });
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(timeout_ms))
        .ok_or_else(|| HarnessError::new(ErrorCode::InvalidPayload, "process timeout overflows"))?;
    // Do not poll `try_wait` before the terminal `wait`: process-wrap's
    // Windows Job Object adapter uses the completion port in `try_wait`, and a
    // consumed completion message would make a later `wait` block forever.
    // The single wait future is raced against timeout/cancellation instead.
    let signal = {
        let wait_future = child.wait();
        tokio::pin!(wait_future);
        tokio::select! {
            result = &mut wait_future => WaitSignal::Exited(result),
            () = cancellation.cancelled() => WaitSignal::Canceled,
            () = sleep(deadline.saturating_duration_since(Instant::now())) => WaitSignal::TimedOut,
        }
    };
    let (status, timed_out, canceled, tree_cleanup) = match signal {
        WaitSignal::Exited(result) => {
            let status = result.map_err(|error| {
                HarnessError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    format!("cannot confirm process tree completion: {error}"),
                )
            })?;
            // The Unix wrapper can report ECHILD after the group leader exits
            // even while orphaned descendants still run in its process group.
            // Kill any such descendants and wait until the group disappears.
            #[cfg(unix)]
            let cleanup = finish_exited_group(&mut child, process_group_id).await?;
            #[cfg(not(unix))]
            let cleanup = TreeCleanup::ReapedOnExit;
            (status, false, false, cleanup)
        }
        WaitSignal::TimedOut => (
            terminate_and_reap(&mut child, "timed-out").await?,
            true,
            false,
            TreeCleanup::KilledAndReaped,
        ),
        WaitSignal::Canceled => (
            terminate_and_reap(&mut child, "canceled").await?,
            false,
            true,
            TreeCleanup::KilledAndReaped,
        ),
    };
    drop(child);
    let stdout = join_reader(stdout_reader).await?;
    let stderr = join_reader(stderr_reader).await?;
    // Framing copies nothing: the bytes are only assembled by a caller that
    // publishes them, and a capture that cannot be assembled then is still a
    // real failure the caller turns into an outcome-unknown receipt.
    let capture = finalize_capture(stdout, stderr, limits);
    Ok(ProcessResult {
        executable: executable.to_owned(),
        shell: None,
        exit_code: status.code(),
        timed_out,
        canceled,
        queued,
        tree_cleanup,
        stdout: capture.stdout_head.clone(),
        stderr: capture.stderr_head.clone(),
        stdout_truncated: capture.stdout_preview_truncated,
        stderr_truncated: capture.stderr_preview_truncated,
        stdout_tail: capture.stdout_tail.clone(),
        stderr_tail: capture.stderr_tail.clone(),
        capture: Some(capture),
    })
}

/// A Unix process group may outlive its leader. The wrapper's `wait` only
/// confirms that our direct child was reaped, so verify the whole group here.
#[cfg(unix)]
async fn finish_exited_group(
    child: &mut Box<dyn process_wrap::tokio::ChildWrapper>,
    group_id: u32,
) -> Result<TreeCleanup, HarnessError> {
    let group = Pid::from_raw(i32::try_from(group_id).map_err(|_| {
        HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "process group ID exceeds i32",
        )
    })?);
    match killpg(group, None) {
        Err(Errno::ESRCH) => return Ok(TreeCleanup::ReapedOnExit),
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("cannot inspect exited process group: {error}"),
            ));
        }
        Ok(()) => {}
    }
    match child.start_kill() {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(Errno::ESRCH as i32) => {}
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("cannot terminate descendants of exited process: {error}"),
            ));
        }
    }
    let deadline = Instant::now() + CLEANUP_BOUND;
    loop {
        match killpg(group, None) {
            Err(Errno::ESRCH) => return Ok(TreeCleanup::KilledAndReaped),
            Err(error) => {
                return Err(HarnessError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    format!("cannot confirm exited process group cleanup: {error}"),
                ));
            }
            Ok(()) if Instant::now() >= deadline => {
                return Err(HarnessError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    "descendants of exited process were not confirmed gone",
                ));
            }
            Ok(()) => sleep(Duration::from_millis(25)).await,
        }
    }
}

/// A call withdrawn while it was queued (or the moment its permit arrived).
///
/// No process exists, so there is nothing to terminate or reap and the tree is
/// trivially clean; `queued` records whether the caller actually waited behind
/// another call or was withdrawn before the runner took it.
fn canceled_before_spawn(executable: &str, queued: bool) -> ProcessResult {
    ProcessResult {
        executable: executable.to_owned(),
        shell: None,
        exit_code: None,
        timed_out: false,
        canceled: true,
        queued,
        tree_cleanup: TreeCleanup::NothingToClean,
        stdout: String::new(),
        stderr: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        stdout_tail: StreamTail::default(),
        stderr_tail: StreamTail::default(),
        capture: None,
    }
}

/// Kill the whole container, then prove it is gone.
///
/// The kill request alone is not evidence: `start_kill` only asks the OS to
/// terminate the tree. Awaiting the backend's `wait` is what confirms every
/// process left it, so an unconfirmed reap is reported as an unknown outcome
/// instead of a settled success.
async fn terminate_and_reap(
    child: &mut Box<dyn process_wrap::tokio::ChildWrapper>,
    reason: &str,
) -> Result<ExitStatus, HarnessError> {
    child.start_kill().map_err(|error| {
        HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!("cannot terminate {reason} process tree: {error}"),
        )
    })?;
    match tokio::time::timeout(CLEANUP_BOUND, child.wait()).await {
        Ok(Ok(status)) => Ok(status),
        Ok(Err(error)) => Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!("cannot reap {reason} process tree: {error}"),
        )),
        Err(_) => Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!(
                "the {reason} process tree was not confirmed empty within {} ms",
                CLEANUP_BOUND.as_millis()
            ),
        )),
    }
}

/// Drain one stream into a spool file, redacting granted values as it goes.
///
/// The redaction happens on the way in, so a granted secret never exists in the
/// spool file, the artifact, or a preview — only in the child's own memory.
async fn read_spooled<R>(
    mut stream: R,
    root: std::path::PathBuf,
    limits: SpoolLimits,
    label: &'static str,
    secrets: Vec<Vec<u8>>,
    progress: Option<ToolProgressSink>,
) -> Result<SpooledStream, HarnessError>
where
    R: AsyncRead + Unpin,
{
    let mut writer = SpoolWriter::create(&root, limits, label)?;
    let mut redactor = Redactor::new(&secrets);
    // Then any value shaped like a key, granted or not: `env` printing a key
    // the host never handed out is the case this catches.
    let mut shaped = crate::secret_values::LineRedactor::default();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| drain_error(&error))?;
        if read == 0 {
            break;
        }
        let safe = shaped.push(&redactor.push(&buffer[..read]));
        writer.write(&safe)?;
        if let Some(progress) = &progress
            && !safe.is_empty()
        {
            progress(&safe);
        }
    }
    let mut tail = shaped.push(&redactor.finish());
    tail.extend(shaped.finish());
    writer.write(&tail)?;
    if let Some(progress) = &progress
        && !tail.is_empty()
    {
        progress(&tail);
    }
    Ok(writer.finish())
}

fn drain_error(error: &io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::ProcessOutcomeUnknown,
        format!("cannot drain process output: {error}"),
    )
}

async fn join_reader(
    reader: Option<JoinHandle<Result<SpooledStream, HarnessError>>>,
) -> Result<SpooledStream, HarnessError> {
    let Some(reader) = reader else {
        return Err(HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            "process output stream was not captured",
        ));
    };
    reader.await.map_err(|error| {
        HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!("process output reader did not complete: {error}"),
        )
    })?
}

#[cfg(test)]
mod queue_tests {
    use super::{ProcessLane, ProcessPermit, ProcessQueue};

    /// Found live on the 0.9.12 candidate (queue.sh): one host-wide queue made
    /// a parent's 15-second sleep wait out its child's 50-second one. A tool
    /// service's model processes queue behind each other only.
    #[tokio::test]
    async fn one_agents_process_does_not_wait_for_anothers() {
        let parent = ProcessQueue::default();
        let child = ProcessQueue::default();
        let running = ProcessPermit::try_acquire(&ProcessLane::Owned(child.clone()))
            .expect("the child's process runs");
        assert!(
            ProcessPermit::try_acquire(&ProcessLane::Owned(parent.clone())).is_some(),
            "the parent's process does not queue behind the child's"
        );
        assert!(
            ProcessPermit::try_acquire(&ProcessLane::Owned(child.clone())).is_none(),
            "the child's next process queues behind its own"
        );
        drop(running);
        assert!(ProcessPermit::try_acquire(&ProcessLane::Owned(child)).is_some());
    }
}
