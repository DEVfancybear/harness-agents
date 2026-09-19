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
fn i03_fixture_route_never_bypasses_the_terminal_or_headless_contract() {
    let sandbox = Sandbox::new();

    // The fixture backend is an explicit opt-in for the interactive app only; it
    // must not make a piped launch succeed.
    let piped = sandbox.run(&["chat", "--fixture"]);
    assert_eq!(piped.code(), 2, "stderr was: {}", piped.stderr);
    assert!(
        piped.stderr.contains("no interactive terminal"),
        "{}",
        piped.stderr
    );

    // A single headless turn must report the real backend state, never a fixture.
    let headless = sandbox.run(&[
        "chat",
        "--headless",
        "--prompt",
        "hello",
        "--fixture",
        "--json",
    ]);
    assert_eq!(headless.code(), 2, "stdout was: {}", headless.stdout);
    assert!(
        headless.stderr.contains("--fixture is only valid"),
        "{}",
        headless.stderr
    );

    // The flag is documented where the operator looks for it.
    let help = sandbox.run(&["chat", "--help"]);
    assert_eq!(help.code(), 0, "stderr was: {}", help.stderr);
    assert!(help.stdout.contains("--fixture"), "{}", help.stdout);
    assert!(help.stdout.contains("--headless"), "{}", help.stdout);

    assert!(
        sandbox.state_entries().is_empty(),
        "refused launches wrote state: {:?}",
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

// ---------------------------------------------------------------------------
// I03/I12 - a headless turn through the real adapter
// ---------------------------------------------------------------------------

/// One-shot SSE fixture server on a real socket.
fn sse_fixture(text: &'static str) -> (String, std::thread::JoinHandle<String>) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("fixture accepts");
        let mut request = vec![0_u8; 8192];
        let read = socket.read(&mut request).expect("fixture reads");
        request.truncate(read);
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket
            .write_all(response.as_bytes())
            .expect("fixture writes");
        socket.flush().ok();
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{address}/chat/completions"), handle)
}

/// Transport failures this sandbox is known to produce intermittently: a
/// freshly started loopback listener refuses a connection for a few seconds.
/// The retry below is keyed on that exact signature only, so a real failure
/// still fails on the first attempt.
const LOOPBACK_WOBBLE: &str = "error sending request for url";

fn attempt_headless_turn() -> Result<(CliRun, String), String> {
    let (endpoint, server) = sse_fixture("fixture says hello");
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let output = std::process::Command::new(cli_binary())
        .args([
            "chat",
            "--headless",
            "--prompt",
            "hello from the headless test",
            "--json",
        ])
        .current_dir(&project)
        .env("HA_HOME", sandbox.path())
        .env("HA_PROVIDER_ENDPOINT", &endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(Stdio::null())
        .output()
        .expect("ha binary runs");
    let run = CliRun::from_output(&output);
    let request = server.join().expect("fixture server finishes");
    if run.code() != 0 && run.stderr.contains(LOOPBACK_WOBBLE) {
        return Err(run.stderr);
    }
    Ok((run, request))
}

#[test]
fn i03_headless_turn_runs_through_the_real_adapter_and_keeps_the_key_out_of_output() {
    let mut attempt = 0;
    let (run, request) = loop {
        attempt += 1;
        match attempt_headless_turn() {
            Ok(success) => break success,
            Err(wobble) if attempt < 3 => {
                eprintln!("attempt {attempt} hit the known loopback wobble: {wobble}");
            }
            Err(wobble) => panic!("loopback fixture never became reachable: {wobble}"),
        }
    };

    assert_eq!(run.code(), 0, "stderr was: {}", run.stderr);
    let parsed: serde_json::Value =
        serde_json::from_str(&run.stdout).expect("headless output is JSON");
    assert_eq!(
        parsed["response"],
        serde_json::Value::String("fixture says hello".to_owned())
    );
    assert_eq!(parsed["fixture"], serde_json::Value::Bool(false));
    assert_eq!(
        parsed["stop"],
        serde_json::Value::String("final".to_owned())
    );
    assert_eq!(
        parsed["approvals"],
        serde_json::Value::String("none".to_owned())
    );

    assert!(
        request.contains("Bearer fixture-secret-value"),
        "the adapter must authenticate: {request}"
    );
    for stream in [&run.stdout, &run.stderr] {
        assert!(
            !stream.contains("fixture-secret-value"),
            "the credential must never be printed: {stream}"
        );
        assert!(
            !stream.contains(char::from(27)),
            "redirected output must stay free of terminal control sequences"
        );
    }
}

#[test]
fn i12_headless_turn_without_provider_configuration_fails_closed() {
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let output = std::process::Command::new(cli_binary())
        .args(["chat", "--headless", "--prompt", "hello", "--json"])
        .current_dir(&project)
        .env("HA_HOME", sandbox.path())
        .env_remove("HA_PROVIDER_ENDPOINT")
        .env_remove("HA_PROVIDER_MODEL")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("HA_API_KEY")
        .stdin(Stdio::null())
        .output()
        .expect("ha binary runs");
    let run = CliRun::from_output(&output);

    assert_ne!(
        run.code(),
        0,
        "an unconfigured provider must not report success"
    );
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    assert!(run.stderr.contains("service_unavailable"), "{}", run.stderr);
    assert!(
        run.stderr.contains("HA_PROVIDER_ENDPOINT"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("DEEPSEEK_API_KEY"), "{}", run.stderr);
    assert!(
        run.stderr.contains("no fixture answer was substituted"),
        "the failure must state that nothing was faked: {}",
        run.stderr
    );
}

/// SSE fixture that serves several requests and returns the full request bodies.
fn sse_fixture_multi(
    text: &'static str,
    requests: usize,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let handle = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for _ in 0..requests {
            let (mut socket, _) = listener.accept().expect("fixture accepts");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            // Read the head, then exactly the declared body.
            let head_end = loop {
                let read = socket.read(&mut chunk).expect("fixture reads");
                if read == 0 {
                    break request.len();
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
            let length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            while request.len() < head_end + length {
                let read = socket.read(&mut chunk).expect("fixture reads body");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let body = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket
                .write_all(response.as_bytes())
                .expect("fixture writes");
            socket.flush().ok();
            bodies.push(String::from_utf8_lossy(&request).into_owned());
        }
        bodies
    });
    (format!("http://{address}/chat/completions"), handle)
}

#[test]
fn i13_resume_continues_the_task_with_recovered_context_and_no_rerun() {
    let (endpoint, server) = sse_fixture_multi("fixture answer", 2);
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let run_headless = |arguments: Vec<&str>| {
        std::process::Command::new(cli_binary())
            .args(arguments)
            .current_dir(&project)
            .env("HA_HOME", sandbox.path())
            .env("HA_PROVIDER_ENDPOINT", &endpoint)
            .env("HA_PROVIDER_MODEL", "fixture-model")
            .env("DEEPSEEK_API_KEY", "fixture-secret-value")
            .stdin(Stdio::null())
            .output()
            .expect("ha binary runs")
    };

    let first = run_headless(vec![
        "chat",
        "--headless",
        "--prompt",
        "first prompt from the test",
        "--json",
    ]);
    let first = CliRun::from_output(&first);
    assert_eq!(first.code(), 0, "stderr was: {}", first.stderr);
    let first_json: serde_json::Value =
        serde_json::from_str(&first.stdout).expect("first output is JSON");
    let session_id = first_json["session_id"]
        .as_str()
        .expect("session id")
        .to_owned();
    let task_id = first_json["task_id"].as_str().expect("task id").to_owned();
    assert_eq!(first_json["tool_calls"], serde_json::Value::from(0));
    assert_eq!(first_json["resumed_from"], serde_json::Value::Null);

    let second = run_headless(vec![
        "chat",
        "--headless",
        "--resume",
        &session_id,
        "--prompt",
        "second prompt from the test",
        "--json",
    ]);
    let second = CliRun::from_output(&second);
    assert_eq!(second.code(), 0, "stderr was: {}", second.stderr);
    let second_json: serde_json::Value =
        serde_json::from_str(&second.stdout).expect("second output is JSON");
    assert_eq!(
        second_json["resumed_from"].as_str(),
        Some(session_id.as_str()),
        "the turn reports what it resumed from"
    );
    assert_eq!(
        second_json["task_id"].as_str(),
        Some(task_id.as_str()),
        "the task identity is kept across the resume"
    );
    assert_ne!(
        second_json["session_id"].as_str(),
        Some(session_id.as_str()),
        "the follow-up owns its own session"
    );
    assert_eq!(second_json["tool_calls"], serde_json::Value::from(0));

    let requests = server.join().expect("fixture server finishes");
    assert_eq!(requests.len(), 2, "one provider call per turn");
    assert!(
        requests[1].contains("first prompt from the test"),
        "the resumed turn must carry the recovered context: {}",
        &requests[1][..requests[1].len().min(600)]
    );
    assert!(
        requests[1].contains("second prompt from the test"),
        "the new input is sent as well"
    );
}

#[test]
fn i13_resuming_an_unknown_session_fails_without_running_anything() {
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let output = std::process::Command::new(cli_binary())
        .args([
            "chat",
            "--headless",
            "--resume",
            "session_0192f0aa-bbcc-7ddd-8eee-000000000001",
            "--prompt",
            "hello",
            "--json",
        ])
        .current_dir(&project)
        .env("HA_HOME", sandbox.path())
        .env(
            "HA_PROVIDER_ENDPOINT",
            "http://127.0.0.1:1/chat/completions",
        )
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(Stdio::null())
        .output()
        .expect("ha binary runs");
    let run = CliRun::from_output(&output);
    assert_ne!(run.code(), 0, "an unknown session must not start a run");
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    assert!(run.stderr.contains("nothing was resumed"), "{}", run.stderr);
}
