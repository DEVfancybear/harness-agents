use std::{
    io,
    path::Path,
    process::{ExitStatus, Stdio},
    sync::OnceLock,
    time::Duration,
};

use harness_providers::CancellationToken;
use harness_types::{ErrorCode, HarnessError};
#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    task::JoinHandle,
    time::{Instant, sleep},
};

use crate::{
    capture::{
        FinalizedCapture, ProcessSpoolConfig, Redactor, SpoolLimits, SpoolWriter, SpooledStream,
        finalize_capture,
    },
    secrets::ProcessEnvironment,
};

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

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub(crate) struct ProcessResult {
    pub executable: String,
    /// Shell selection for `run_shell`; structured process calls leave this absent.
    pub shell: Option<String>,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    /// Whether this call had to wait for the host-wide process permit.
    ///
    /// It is recorded so a canceled queue member is distinguishable from a call
    /// that was withdrawn before it ever reached the runner.
    pub queued: bool,
    pub tree_cleanup: TreeCleanup,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    /// The spooled capture, when the call actually ran a process. It is the
    /// durable copy of everything the process wrote, bounded by the quota.
    pub capture: Option<FinalizedCapture>,
}

pub(crate) async fn run_structured(
    root: &Path,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
    environment: &ProcessEnvironment,
    spool: &ProcessSpoolConfig,
) -> Result<ProcessResult, HarnessError> {
    run_structured_with_host(
        root,
        executable,
        args,
        timeout_ms,
        cancellation,
        environment,
        spool,
        &HostEnvironment::from_process(),
    )
    .await
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
    )
    .await?;
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
    #[cfg(windows)]
    let (executable, shell) = windows_shell(host);
    #[cfg(windows)]
    let args = vec![
        "-NoProfile".to_owned(),
        "-NonInteractive".to_owned(),
        "-Command".to_owned(),
        command.to_owned(),
    ];
    #[cfg(not(windows))]
    let (executable, args) = ("sh".to_owned(), vec!["-c".to_owned(), command.to_owned()]);
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
    )
    .await?;
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
    if host.lookup("PATH").is_some_and(|path| {
        path.split(';').any(|directory| {
            let directory = directory.trim_matches('"');
            Path::new(directory).join("pwsh.exe").is_file()
        })
    }) {
        ("pwsh".to_owned(), "powershell-7".to_owned())
    } else {
        (
            "powershell.exe".to_owned(),
            "powershell-5.1 (pwsh not found)".to_owned(),
        )
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
        let result = run_shell_with_host(
            workspace.path(),
            "Write-Output 'shell-fallback-ok'",
            10_000,
            CancellationToken::new(),
            &ProcessEnvironment::empty(),
            &ProcessSpoolConfig::default(),
            &host,
        )
        .await
        .expect("fallback starts");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(
            result.shell.as_deref(),
            Some("powershell-5.1 (pwsh not found)")
        );
        assert!(result.stdout.contains("shell-fallback-ok"));
    }
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
) -> Result<ProcessResult, HarnessError> {
    // Windows Job Object completion ports are process-lifecycle resources. A
    // single host-wide runner permit makes concurrent tool calls deterministic
    // and prevents two cleanup waits from starving each other; it does not
    // bypass the per-process tree ownership or output bounds.
    //
    // The queue is cancellation-aware: a call withdrawn by its caller must not
    // start a process later, when the permit finally reaches it. The fast path
    // keeps the uncontended case free of a select branch.
    let (guard, queued) = if let Ok(guard) = process_execution_lock().try_lock() {
        (guard, false)
    } else {
        let guard = tokio::select! {
            guard = process_execution_lock().lock() => guard,
            () = cancellation.cancelled() => {
                return Ok(canceled_before_spawn(executable, true));
            }
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
    let mut command = CommandWrap::with_new(executable, |child_command| {
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
        for (name, value) in environment.values() {
            child_command.env(name, value);
        }
    });
    command.wrap(KillOnDrop);
    #[cfg(windows)]
    command.wrap(JobObject);
    #[cfg(unix)]
    command.wrap(ProcessSession);
    let mut child = command.spawn().map_err(|error| {
        HarnessError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!("cannot spawn structured process: {error}"),
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
        ))
    });
    let stderr_reader = stderr.map(|stream| {
        tokio::spawn(read_spooled(
            stream,
            spool.root().to_owned(),
            limits,
            "stderr",
            environment.redactions().to_vec(),
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
            // This only proves that the direct child exited. The Windows job
            // wrapper can report its root process before detached descendants
            // finish; dropping the wrapper requests kill-on-close, but that is
            // not a backend confirmation that the job is empty.
            (status, false, false, TreeCleanup::ReapedOnExit)
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
    // A capture that cannot be assembled is a real failure, but the process has
    // already run: the caller turns this into an outcome-unknown receipt rather
    // than pretending no side effect happened.
    let capture = finalize_capture(stdout, stderr, limits)?;
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
        capture: Some(capture),
    })
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
) -> Result<SpooledStream, HarnessError>
where
    R: AsyncRead + Unpin,
{
    let mut writer = SpoolWriter::create(&root, limits, label)?;
    let mut redactor = Redactor::new(&secrets);
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = stream
            .read(&mut buffer)
            .await
            .map_err(|error| drain_error(&error))?;
        if read == 0 {
            break;
        }
        let safe = redactor.push(&buffer[..read]);
        writer.write(&safe)?;
    }
    let tail = redactor.finish();
    writer.write(&tail)?;
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
