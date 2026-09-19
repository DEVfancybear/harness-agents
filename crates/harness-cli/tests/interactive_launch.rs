//! `HA_LAUNCH` H01 acceptance: the dispatch contract, the terminal guard and
//! parser compatibility, observed through the compiled binary as a real process.
//!
//! Nothing here claims the interactive UI (H03) or the agent service (H04). The
//! PTY-backed I01 transcript belongs to H07; every assertion below holds with
//! redirected stdio, which is exactly what these cases exercise.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Resolve the compiled ha executable this crate produced.
fn cli_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_ha") {
        let candidate = PathBuf::from(path);
        assert!(
            candidate.is_file(),
            "compiled ha binary missing at {}",
            candidate.display()
        );
        return candidate;
    }
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.is_file(),
        "compiled ha binary missing at {}",
        candidate.display()
    );
    candidate
}

/// Isolated state root, so a fast path can be proven to have written nothing.
struct Sandbox {
    root: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("temporary HA_HOME"),
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn command<S: AsRef<OsStr>>(&self, arguments: &[S]) -> Command {
        let mut command = Command::new(cli_binary());
        command
            .args(arguments)
            .current_dir(self.path())
            .env("HA_HOME", self.path());
        command
    }

    /// Run with both streams redirected and stdin already at end of file.
    fn run<S: AsRef<OsStr>>(&self, arguments: &[S]) -> CliRun {
        let output = self
            .command(arguments)
            .stdin(Stdio::null())
            .output()
            .expect("ha binary runs");
        CliRun::from_output(&output)
    }

    /// Run while keeping the stdin pipe open, to prove the guard never waits.
    fn run_with_open_stdin<S: AsRef<OsStr>>(&self, arguments: &[S]) -> CliRun {
        let mut child = self
            .command(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("ha binary spawns");
        let held_stdin = child.stdin.take().expect("stdin pipe");
        let status = wait_bounded(&mut child, Duration::from_secs(30));
        drop(held_stdin);
        let status = status.unwrap_or_else(|| {
            let _ = child.kill();
            panic!("ha kept waiting for input while stdin stayed open");
        });
        CliRun::from_child(child, status)
    }

    fn state_entries(&self) -> Vec<String> {
        let mut entries = std::fs::read_dir(self.path())
            .expect("sandbox is readable")
            .map(|entry| {
                entry
                    .expect("sandbox entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }
}

fn wait_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("wait on ha") {
            return Some(status);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Captured result of one CLI run.
struct CliRun {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl CliRun {
    fn from_output(output: &std::process::Output) -> Self {
        Self {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn from_child(mut child: Child, status: ExitStatus) -> Self {
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stdout.take() {
            pipe.read_to_string(&mut stdout)
                .expect("stdout is readable");
        }
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_string(&mut stderr)
                .expect("stderr is readable");
        }
        Self {
            status,
            stdout,
            stderr,
        }
    }

    fn code(&self) -> i32 {
        self.status.code().expect("ha exits with a status code")
    }
}

fn repository_fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

// ---------------------------------------------------------------------------
// I03 - non-terminal launch must fail fast with instructions
// ---------------------------------------------------------------------------

#[test]
fn i03_bare_launch_without_a_terminal_exits_two_with_instructions() {
    let sandbox = Sandbox::new();
    let run = sandbox.run_with_open_stdin::<&str>(&[]);
    assert_eq!(run.code(), 2, "stderr was: {}", run.stderr);
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    assert!(
        run.stderr.contains("no interactive terminal"),
        "{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("ha chat --headless --prompt"),
        "guidance must point at the explicit headless command: {}",
        run.stderr
    );
    assert!(
        sandbox.state_entries().is_empty(),
        "a refused launch wrote state"
    );
}

#[test]
fn i01_chat_without_a_terminal_uses_the_same_guard() {
    let sandbox = Sandbox::new();
    let run = sandbox.run(&["chat"]);
    assert_eq!(run.code(), 2, "stderr was: {}", run.stderr);
    assert!(
        run.stderr.contains("no interactive terminal"),
        "{}",
        run.stderr
    );
}

// ---------------------------------------------------------------------------
// I02 - fast paths and legacy dispatch
// ---------------------------------------------------------------------------

#[test]
fn i02_help_and_version_stay_fast_paths_that_write_nothing() {
    let sandbox = Sandbox::new();
    let help = sandbox.run(&["--help"]);
    assert_eq!(help.code(), 0, "stderr was: {}", help.stderr);
    assert!(
        help.stdout.contains("chat"),
        "help must list chat: {}",
        help.stdout
    );
    assert!(help.stdout.contains("Usage"), "{}", help.stdout);

    let version = sandbox.run(&["--version"]);
    assert_eq!(version.code(), 0, "stderr was: {}", version.stderr);
    assert!(
        version.stdout.contains(env!("CARGO_PKG_VERSION")),
        "{}",
        version.stdout
    );

    for arguments in [
        vec!["memory", "--help"],
        vec!["maintenance", "--help"],
        vec!["chat", "--help"],
    ] {
        let run = sandbox.run(&arguments);
        assert_eq!(run.code(), 0, "{arguments:?} failed: {}", run.stderr);
    }

    assert!(
        sandbox.state_entries().is_empty(),
        "a fast path wrote state: {:?}",
        sandbox.state_entries()
    );
}

#[test]
fn i02_existing_subcommands_keep_their_dispatch_and_output() {
    let sandbox = Sandbox::new();
    let config = repository_fixture("tests/fixtures/p0/config/valid.toml");
    let validate = sandbox.run(&[
        OsStr::new("config"),
        OsStr::new("validate"),
        OsStr::new("--config"),
        config.as_os_str(),
    ]);
    assert_eq!(validate.code(), 0, "stderr was: {}", validate.stderr);
    assert!(
        validate.stdout.contains("config valid"),
        "{}",
        validate.stdout
    );

    let json = sandbox.run(&[
        OsStr::new("config"),
        OsStr::new("validate"),
        OsStr::new("--config"),
        config.as_os_str(),
        OsStr::new("--json"),
    ]);
    assert_eq!(json.code(), 0, "stderr was: {}", json.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(&json.stdout).expect("config validate --json stays valid JSON");
    assert_eq!(parsed["valid"], serde_json::Value::Bool(true));

    assert!(
        sandbox.state_entries().is_empty(),
        "config validate wrote state"
    );
}

#[test]
fn i02_unknown_options_and_missing_arguments_remain_parser_errors() {
    let sandbox = Sandbox::new();
    let unknown = sandbox.run(&["--definitely-not-an-option"]);
    assert_eq!(unknown.code(), 2, "stderr was: {}", unknown.stderr);
    assert!(
        unknown.stderr.contains("unexpected argument"),
        "{}",
        unknown.stderr
    );

    let missing = sandbox.run(&["run"]);
    assert_eq!(missing.code(), 2, "stderr was: {}", missing.stderr);
    assert!(missing.stderr.contains("required"), "{}", missing.stderr);
}

// ---------------------------------------------------------------------------
// I03 - headless contract
// ---------------------------------------------------------------------------

#[test]
fn i03_headless_rejects_the_headless_only_flags_and_keeps_stdout_plain() {
    let sandbox = Sandbox::new();
    for arguments in [
        vec!["chat", "--headless"],
        vec!["chat", "--prompt", "hello"],
        vec!["chat", "--json"],
        vec!["chat", "--prompt", "hello", "--json"],
    ] {
        let run = sandbox.run(&arguments);
        assert_eq!(run.code(), 2, "{arguments:?} was accepted: {}", run.stdout);
    }

    let headless = sandbox.run(&["chat", "--headless", "--prompt", "hello", "--json"]);
    assert!(
        !headless.stdout.contains(char::from(27)),
        "redirected headless output must not contain terminal control sequences"
    );
    assert_ne!(
        headless.code(),
        0,
        "an unwired headless turn reported success"
    );
    assert!(
        headless.stderr.contains("service_unavailable"),
        "the staged state must be a typed error, not a fabricated response: {}",
        headless.stderr
    );
    assert!(
        sandbox.state_entries().is_empty(),
        "a refused headless turn wrote state"
    );
}
