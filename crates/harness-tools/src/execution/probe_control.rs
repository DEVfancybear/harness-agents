//! The negative controls for the M12 capability probes.
//!
//! A probe that only ever runs against the production path proves nothing: it
//! cannot distinguish "the boundary holds" from "the fixture never tried". So
//! each boundary probe has a control here that runs the *same fixture* with the
//! one wrapper that provides the boundary deliberately omitted — no job object,
//! no environment clearing, no allowlist.
//!
//! This module is fault injection at a real component boundary, and it is the
//! only place in the crate that starts a process outside the P3 gate. It is
//! never reachable from action execution: no production function calls it, and
//! the escalation it exists to detect is exactly what action execution must
//! never do.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use harness_types::{ErrorCode, HarnessError};
use tokio::{io::AsyncReadExt, process::Command};

/// A command this control runs, split so the caller owns the fixture path.
#[derive(Clone, Debug)]
pub struct ControlCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlObservation {
    pub exit_code: Option<i32>,
    pub killed: bool,
    pub stdout: String,
    pub stderr: String,
    /// Wall time from starting the fixture to having it reaped and drained.
    pub elapsed_ms: u128,
}

impl ControlCommand {
    #[must_use]
    pub fn new(executable: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            args: Vec::new(),
            root: root.into(),
        }
    }

    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = String>) -> Self {
        self.args = args.into_iter().collect();
        self
    }
}

/// Run a fixture with the host environment inherited and nothing wrapped.
///
/// This is the "host fallback" a strict request must never silently become: the
/// child inherits the full environment and belongs to no job, so nothing
/// reaps it. It exists to be *observed escaping*.
pub async fn run_inheriting(
    command: &ControlCommand,
    extra_environment: &[(String, String)],
    timeout: Duration,
) -> Result<ControlObservation, HarnessError> {
    let started = std::time::Instant::now();
    let mut child = Command::new(&command.executable)
        .args(&command.args)
        .current_dir(&command.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .envs(extra_environment.iter().cloned())
        .spawn()
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("control fixture cannot start: {error}"),
            )
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let status = tokio::time::timeout(timeout, child.wait()).await;
    let exit_code = match status {
        Ok(Ok(status)) => status.code(),
        Ok(Err(error)) => {
            return Err(HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("control fixture cannot be reaped: {error}"),
            ));
        }
        Err(_) => None,
    };
    let stdout = read_all(stdout, FULL_DRAIN_BOUND).await;
    let stderr = read_all(stderr, FULL_DRAIN_BOUND).await;
    Ok(ControlObservation {
        exit_code,
        killed: false,
        stdout,
        stderr,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Run a fixture, then kill only the process this control started.
///
/// Nothing reaps the descendants, which is the point: if a descendant outlives
/// the kill and keeps writing, the boundary the production path relies on is
/// what was holding it in, not the fixture's own good behaviour.
pub async fn run_then_kill_direct_child(
    command: &ControlCommand,
    extra_environment: &[(String, String)],
    run_for: Duration,
) -> Result<ControlObservation, HarnessError> {
    let started = std::time::Instant::now();
    let mut child = Command::new(&command.executable)
        .args(&command.args)
        .current_dir(&command.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(false)
        .envs(extra_environment.iter().cloned())
        .spawn()
        .map_err(|error| {
            HarnessError::new(
                ErrorCode::ProcessOutcomeUnknown,
                format!("control fixture cannot start: {error}"),
            )
        })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    tokio::time::sleep(run_for).await;
    let killed = child.start_kill().is_ok();
    let exit_code = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.code(),
        _ => None,
    };
    let stdout = read_all(stdout, DRAIN_BOUND).await;
    let stderr = read_all(stderr, DRAIN_BOUND).await;
    Ok(ControlObservation {
        exit_code,
        killed,
        stdout,
        stderr,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Run a fixture that inherits the real environment plus the probe canary.
pub async fn run_without_allowlist(
    command: &ControlCommand,
    canary: &[(String, String)],
    timeout: Duration,
) -> Result<ControlObservation, HarnessError> {
    run_inheriting(command, canary, timeout).await
}

/// Ask the platform for its own version string.
///
/// Run through the control path on purpose: the host identity must not depend
/// on the containment path working, or a broken boundary would silently become
/// an unknown host.
///
/// The identity is *read from the machine every time*, never written down. On
/// Windows the reported product caption and the registry `ProductName` disagree
/// on current builds (the registry value was not updated for Windows 11), so
/// what is recorded is the build the platform reports plus its display version,
/// which is the part that actually identifies the host under test.
pub async fn observe_platform_version(root: &Path) -> Result<String, HarnessError> {
    #[cfg(windows)]
    {
        let reported = run_and_read(root, "cmd", &["/C", "ver"]).await?;
        let version = extract_between(&reported, "[Version ", "]")
            .unwrap_or_else(|| reported.trim().to_owned());
        let display = registry_value(root, "DisplayVersion").await;
        let build = registry_value(root, "CurrentBuild").await;
        let mut identity = version;
        if let Some(build) = build
            && !identity.contains(&build)
        {
            identity = format!("{identity} build {build}");
        }
        if let Some(display) = display {
            identity = format!("{identity} ({display})");
        }
        Ok(identity)
    }
    #[cfg(not(windows))]
    {
        let reported = run_and_read(root, "uname", &["-sr"]).await?;
        Ok(reported.trim().replace(['\r', '\n'], " "))
    }
}

/// Read a `REG_SZ` value under the Windows version key, if it is there.
#[cfg(windows)]
async fn registry_value(root: &Path, name: &str) -> Option<String> {
    let command = ControlCommand::new("reg", root).with_args([
        "query".to_owned(),
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion".to_owned(),
        "/v".to_owned(),
        name.to_owned(),
    ]);
    let observation = run_inheriting(&command, &[], Duration::from_secs(10))
        .await
        .ok()?;
    observation
        .stdout
        .lines()
        .find_map(|line| line.split_once("REG_SZ"))
        .map(|(_, value)| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

async fn run_and_read(
    root: &Path,
    executable: &str,
    args: &[&str],
) -> Result<String, HarnessError> {
    let command = ControlCommand::new(executable, root)
        .with_args(args.iter().map(|value| (*value).to_owned()));
    let observation = run_inheriting(&command, &[], Duration::from_secs(10)).await?;
    Ok(format!("{} {}", observation.stdout, observation.stderr))
}

// Only the Windows probes read tagged output; on other hosts it has no caller, and
// the CI clippy gate (`-D warnings`) failed the Linux job on the dead code.
#[cfg(windows)]
fn extract_between(text: &str, start: &str, end: &str) -> Option<String> {
    let (_, rest) = text.split_once(start)?;
    let (value, _) = rest.split_once(end)?;
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// How long a control waits for a fixture's output before giving up on EOF.
///
/// A bound is required, not tidiness: on Windows a descendant inherits the
/// parent's *inheritable* handles, so a grandchild holds the write end of the
/// output pipe open long after the process the control killed. Reading to EOF
/// would therefore wait for the whole tree, which is the opposite of what a
/// control that measures "did the tree survive?" needs.
const DRAIN_BOUND: Duration = Duration::from_millis(500);

/// A normal fixture's output is read to EOF within this bound.
const FULL_DRAIN_BOUND: Duration = Duration::from_secs(5);

async fn read_all<R>(stream: Option<R>, bound: Duration) -> String
where
    R: AsyncReadExt + Unpin,
{
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let deadline = tokio::time::Instant::now() + bound;
    loop {
        let mut buffer = [0_u8; 8 * 1024];
        match tokio::time::timeout_at(deadline, stream.read(&mut buffer)).await {
            Ok(Ok(read)) if read > 0 => bytes.extend_from_slice(&buffer[..read]),
            // EOF, a read error, and the bound expiring all end the drain: what
            // was read before them is still evidence.
            _ => break,
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Terminate processes a control deliberately left running.
///
/// A control that proves an escape has to bound the leak it created: the
/// fixture writes its descendants' pids down, and the control kills exactly
/// those. It never kills by image name.
pub async fn terminate_pids(root: &Path, pids: &[u32]) -> usize {
    let mut ended = 0;
    for pid in pids {
        #[cfg(windows)]
        let command = ControlCommand::new("taskkill", root).with_args([
            "/F".to_owned(),
            "/PID".to_owned(),
            pid.to_string(),
        ]);
        #[cfg(not(windows))]
        let command =
            ControlCommand::new("kill", root).with_args(["-9".to_owned(), pid.to_string()]);
        if run_inheriting(&command, &[], Duration::from_secs(10))
            .await
            .is_ok_and(|observation| observation.exit_code == Some(0))
        {
            ended += 1;
        }
    }
    ended
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The control must be able to lose: if it could not start a fixture, every
    /// capability probe that depends on it would report a false enforcement.
    #[tokio::test]
    async fn a_missing_fixture_is_an_error_and_not_an_enforcement() {
        let temp = tempfile::tempdir().expect("temp root");
        let command = ControlCommand::new(temp.path().join("definitely-absent"), temp.path());
        assert!(
            run_inheriting(&command, &[], Duration::from_secs(5))
                .await
                .is_err()
        );
    }
}
