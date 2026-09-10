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
    io::{AsyncRead, AsyncReadExt},
    task::JoinHandle,
    time::{Instant, sleep},
};

const PROCESS_OUTPUT_LIMIT: usize = 64 * 1024;

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
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub canceled: bool,
    pub tree_cleanup_confirmed: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub(crate) async fn run_structured(
    root: &Path,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> Result<ProcessResult, HarnessError> {
    run(root, executable, args, timeout_ms, cancellation).await
}

pub(crate) async fn run_shell(
    root: &Path,
    command: &str,
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> Result<ProcessResult, HarnessError> {
    #[cfg(windows)]
    let (executable, args) = (
        "pwsh".to_owned(),
        vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-Command".to_owned(),
            command.to_owned(),
        ],
    );
    #[cfg(not(windows))]
    let (executable, args) = ("sh".to_owned(), vec!["-c".to_owned(), command.to_owned()]);
    run(root, &executable, &args, timeout_ms, cancellation).await
}

async fn run(
    root: &Path,
    executable: &str,
    args: &[String],
    timeout_ms: u64,
    cancellation: CancellationToken,
) -> Result<ProcessResult, HarnessError> {
    // Windows Job Object completion ports are process-lifecycle resources. A
    // single host-wide runner lock makes concurrent tool calls deterministic
    // and prevents two cleanup waits from starving each other; it does not
    // bypass the per-process tree ownership or output bounds.
    let _execution_guard = process_execution_lock().lock().await;
    let mut command = CommandWrap::with_new(executable, |child_command| {
        child_command
            .args(args)
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    let stdout_reader = stdout.map(|stream| tokio::spawn(read_bounded(stream)));
    let stderr_reader = stderr.map(|stream| tokio::spawn(read_bounded(stream)));
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
    let (status, timed_out, canceled) = match signal {
        WaitSignal::Exited(result) => {
            let status = result.map_err(|error| {
                HarnessError::new(
                    ErrorCode::ProcessOutcomeUnknown,
                    format!("cannot confirm process tree completion: {error}"),
                )
            })?;
            (status, false, false)
        }
        WaitSignal::TimedOut => (
            terminate_and_reap(&mut child, "timed-out").await?,
            true,
            false,
        ),
        WaitSignal::Canceled => (
            terminate_and_reap(&mut child, "canceled").await?,
            false,
            true,
        ),
    };
    drop(child);
    let stdout = join_reader(stdout_reader).await?;
    let stderr = join_reader(stderr_reader).await?;
    Ok(ProcessResult {
        executable: executable.to_owned(),
        exit_code: status.code(),
        timed_out,
        canceled,
        tree_cleanup_confirmed: true,
        stdout: String::from_utf8_lossy(&stdout.bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
        stdout_truncated: stdout.truncated,
        stderr_truncated: stderr.truncated,
    })
}

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
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("cannot reap {reason} process tree: {error}"),
            )
        })? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("{reason} process tree did not become reapable within the cleanup bound"),
            ));
        }
        sleep(Duration::from_millis(10)).await;
    }
}

#[derive(Debug)]
struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded<R>(mut stream: R) -> io::Result<BoundedBytes>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let room = PROCESS_OUTPUT_LIMIT.saturating_sub(bytes.len());
        if room == 0 {
            truncated = true;
            continue;
        }
        let take = room.min(read);
        bytes.extend_from_slice(&buffer[..take]);
        if take < read {
            truncated = true;
        }
    }
    Ok(BoundedBytes { bytes, truncated })
}

async fn join_reader(
    reader: Option<JoinHandle<io::Result<BoundedBytes>>>,
) -> Result<BoundedBytes, HarnessError> {
    let Some(reader) = reader else {
        return Ok(BoundedBytes {
            bytes: Vec::new(),
            truncated: false,
        });
    };
    reader
        .await
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("process output reader did not complete: {error}"),
            )
        })?
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("cannot drain process output: {error}"),
            )
        })
}
