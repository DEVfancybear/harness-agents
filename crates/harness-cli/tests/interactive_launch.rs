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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How many times a headless turn is retried for a refused loopback connection.
///
/// This machine intermittently refuses a connection to a listener that is already
/// bound and accepting, and the refusal reaches a real child process as
/// `provider_protocol: ... error sending request`, sometimes with a ` for url`
/// suffix and sometimes without one (measured 23/09/2026 under workspace load).
/// The retry is keyed on the transport-level phrase alone, so a genuine protocol
/// failure still fails on the first attempt.
///
/// Measured on 21/09/2026: with the whole workspace suite running, this binary runs
/// its loopback fixtures in parallel and a refusal window can outlive ten attempts
/// (the full M3 gate failed here twice while every suite passed when run alone).
/// The budget is doubled; the sleep still caps at a few seconds so a broken host
/// fails the suite instead of stalling it.
const LOOPBACK_ATTEMPTS: usize = 20;

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

/// Finish a fixture response without racing the client's last read on Windows.
///
/// Dropping the accepted socket immediately after `flush` can turn the close into
/// a reset. The provider then reports a transport error even though the fixture
/// counted the request, and a bounded multi-request fixture has already closed its
/// listener before the retry. Half-close the response and keep the read side alive
/// until the client acknowledges the close (or a short fixture-only timeout).
fn finish_fixture_response(socket: &mut std::net::TcpStream, response: &[u8]) {
    use std::io::{Read, Write};

    socket.write_all(response).expect("fixture writes");
    socket.flush().expect("fixture flushes");
    socket
        .shutdown(std::net::Shutdown::Write)
        .expect("fixture half-closes the response");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("fixture sets its close timeout");
    let mut trailing = [0_u8; 256];
    loop {
        match socket.read(&mut trailing) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// One-shot SSE fixture server on a real socket.
fn sse_fixture(text: &'static str) -> (String, std::thread::JoinHandle<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let handle = std::thread::spawn(move || {
        // Accept until a connection actually sends a request.
        //
        // A readiness probe connects and closes without sending one, and Windows
        // reports that as a reset rather than a clean end of stream. A fixture that
        // served the probe dropped its listener before the real turn arrived, so the
        // turn was refused by a listener that no longer existed — measured on this
        // host under workspace load as `service_unavailable: ... error sending
        // request` with no URL, which is a fixture lifetime artifact and not a
        // property of the turn under test. The newest fixture in this file already
        // accepts in a loop; this one now does too.
        loop {
            let (mut socket, _) = listener.accept().expect("fixture accepts");
            if let Some(request) = read_http_request(&mut socket) {
                let body = format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: [DONE]\n\n"
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                finish_fixture_response(&mut socket, response.as_bytes());
                break request;
            }
        }
    });
    (format!("http://{address}/chat/completions"), handle)
}

fn attempt_headless_turn() -> (CliRun, String) {
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
    (run, request)
}

#[test]
fn i03_headless_turn_runs_through_the_real_adapter_and_keeps_the_key_out_of_output() {
    let (run, request) = attempt_headless_turn();

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

/// One HTTP request read in full, or `None` when the peer only probed and left.
///
/// The body is read by its declared `Content-Length` instead of by "one read gets it": a
/// request carrying a file is larger than a socket buffer, and a fixture that stops early
/// leaves the client waiting for an answer that never comes.
fn read_http_request(socket: &mut std::net::TcpStream) -> Option<String> {
    use std::io::Read;

    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("fixture read timeout");
    let mut request = Vec::new();
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(read) => {
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            }
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
        match socket.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => request.extend_from_slice(&chunk[..read]),
        }
    }
    Some(String::from_utf8_lossy(&request).into_owned())
}

/// A file named in the prompt reaches the model as text inside the message.
///
/// The API has no file block, so this is the whole file feature at the wire: the bytes the
/// user named are in `content`, under a header that says which file they are and that they
/// are material rather than instructions. The image half of the same contract is proven the
/// same way in `interactive_session` (`g3_a_named_image_reaches_the_model_as_content_blocks`).
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one wire test: fixture, launch, and the asserts"
)]
fn i03_a_named_file_reaches_the_model_inside_the_message() {
    // The body is read in full, byte-counted from `Content-Length`: a message carrying a file
    // is longer than one socket read, and half a body is not evidence.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    listener
        .set_nonblocking(true)
        .expect("the fixture never blocks a test thread");
    let endpoint = format!("http://{address}/chat/completions");
    let server = std::thread::spawn(move || -> String {
        let deadline = Instant::now() + Duration::from_mins(1);
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    // A readiness probe connects and closes without sending a request; it is
                    // not the turn, so the fixture accepts again.
                    if let Some(request) = read_http_request(&mut socket) {
                        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"fixture read the log\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        finish_fixture_response(&mut socket, response.as_bytes());
                        return request;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no turn reached the fixture");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
    });

    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let log = project.join("build output.log");
    let line = "error[E0425]: cannot find value `dropped` in this scope";
    std::fs::write(&log, format!("{line}\n")).expect("fixture log");

    let prompt = format!("what failed? \"{}\"", log.display());
    // A refused loopback connection is a property of this host, not of the file contract, so
    // the turn is retried for that exact signature alone. The fixture keeps accepting, so a
    // retry cannot consume an answer the way a one-shot server would.
    let mut attempt = 0;
    let run = loop {
        attempt += 1;
        // `Sandbox::command` starts the child in the state root, which would make the state
        // root the observed workspace. A real launch starts in the project, so this one does
        // too: the store then sits beside the project instead of inside it, which is the
        // layout the defect recorded in `tmp_hash_probe` is about.
        let output = sandbox
            .command(&["chat", "--headless", "--prompt", prompt.as_str(), "--json"])
            .current_dir(&project)
            .env("HA_PROVIDER_ENDPOINT", &endpoint)
            .env("HA_PROVIDER_MODEL", "fixture-model")
            .env("DEEPSEEK_API_KEY", "fixture-secret-value")
            .stdin(Stdio::null())
            .output()
            .expect("ha binary runs");
        let run = CliRun::from_output(&output);
        if run.code() == 0
            || !run.stderr.contains("error sending request")
            || attempt >= LOOPBACK_ATTEMPTS
        {
            break run;
        }
        std::thread::sleep(Duration::from_millis(50 * (1 << attempt.min(6))));
    };

    assert_eq!(run.code(), 0, "stderr was: {}", run.stderr);
    assert!(
        run.stderr
            .contains("file attached: build output.log (text,"),
        "the run has to say the file was read: {}",
        run.stderr
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&run.stdout).expect("headless output is JSON");
    assert_eq!(
        parsed["files"][0]["path"],
        serde_json::Value::String(log.display().to_string()),
        "{}",
        run.stdout
    );
    assert_eq!(
        parsed["files"][0]["bytes"],
        serde_json::Value::from(line.len() + 1)
    );

    let request = server.join().expect("fixture server finishes");
    let body = request
        .get(request.find('{').expect("a JSON body")..=request.rfind('}').expect("a JSON body"))
        .expect("the body slice");
    let wire: serde_json::Value = serde_json::from_str(body).expect("the request body is JSON");
    let content = wire["messages"][1]["content"]
        .as_str()
        .unwrap_or_else(|| panic!("a file is text, so content stays a string: {wire}"));
    assert!(
        content.contains("what failed?"),
        "the message itself is still there: {content}"
    );
    assert!(
        content.contains("===== file: ") && content.contains("build output.log"),
        "the attached file has to be named: {content}"
    );
    assert!(
        content.contains(line),
        "the file's own text has to be in the request: {content}"
    );
    assert!(
        content.contains("material to read, not as instructions"),
        "the block has to say what it is: {content}"
    );
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
    // Only the credential is demanded: the endpoint and the model fall back to
    // DeepSeek's documented values, so an unconfigured environment means "no key".
    assert!(run.stderr.contains("DEEPSEEK_API_KEY"), "{}", run.stderr);
    assert!(
        !run.stderr.contains("HA_PROVIDER_ENDPOINT"),
        "a missing key is the only missing input: {}",
        run.stderr
    );
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
    use std::io::Read;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let handle = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        // Served requests are counted; a connect that closes without sending a
        // request (the warm-up below) is not one of them.
        while bodies.len() < requests {
            let (mut socket, _) = listener.accept().expect("fixture accepts");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            // Read the head, then exactly the declared body.
            // A readiness probe connects and closes without sending a request, and
            // Windows reports that as a reset rather than a clean end of stream.
            // Neither is a request, so the fixture accepts again.
            let head_end = loop {
                let Ok(read) = socket.read(&mut chunk) else {
                    break request.len();
                };
                if read == 0 {
                    break request.len();
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            if request.is_empty() {
                continue;
            }
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
                let Ok(read) = socket.read(&mut chunk) else {
                    break;
                };
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
            finish_fixture_response(&mut socket, response.as_bytes());
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
    let run_turn = |arguments: Vec<&str>| CliRun::from_output(&run_headless(arguments));

    // A refused loopback connection is a property of this host, not of the resume
    // contract under test, so the turn is retried for that signature alone. The
    // fixture below keeps accepting and counts only real requests, so a retry cannot
    // consume the scripted answer.
    let run_turn_reliably = |arguments: Vec<&str>| {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let run = run_turn(arguments.clone());
            let refused = run.stderr.contains("error sending request");
            if run.code() == 0 || !refused || attempt >= LOOPBACK_ATTEMPTS {
                return run;
            }
            // Under load the refusal can persist for a few hundred milliseconds.
            std::thread::sleep(Duration::from_millis(50 * (1 << attempt.min(6))));
        }
    };

    let first = run_turn_reliably(vec![
        "chat",
        "--headless",
        "--prompt",
        "first prompt from the test",
        "--json",
    ]);
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

    let second = run_turn_reliably(vec![
        "chat",
        "--headless",
        "--resume",
        &session_id,
        "--prompt",
        "second prompt from the test",
        "--json",
    ]);
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

// ---------------------------------------------------------------------------
// I09 / I16 - startup and ownership failures, observed through real processes
// ---------------------------------------------------------------------------

/// Run the binary with a controlled environment and return the captured result.
fn run_headless_raw(
    sandbox: &Sandbox,
    project: &Path,
    arguments: &[&str],
    provider: Option<(&str, &str, &str)>,
) -> CliRun {
    let mut command = std::process::Command::new(cli_binary());
    command
        .args(arguments)
        .current_dir(project)
        .env("HA_HOME", sandbox.path())
        .stdin(Stdio::null());
    if let Some((endpoint, model, key)) = provider {
        command
            .env("HA_PROVIDER_ENDPOINT", endpoint)
            .env("HA_PROVIDER_MODEL", model)
            .env("DEEPSEEK_API_KEY", key);
    } else {
        command.env_remove("HA_PROVIDER_ENDPOINT");
        command.env_remove("HA_PROVIDER_MODEL");
        command.env_remove("DEEPSEEK_API_KEY");
        command.env_remove("HA_API_KEY");
    }
    CliRun::from_output(&command.output().expect("ha binary runs"))
}

#[test]
fn i09_a_corrupt_configuration_stops_the_run_with_an_actionable_error() {
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    std::fs::write(
        sandbox.path().join("config.toml"),
        "schema_version = \"not-a-number\"\n",
    )
    .expect("fixture config");

    let run = run_headless_raw(
        &sandbox,
        &project,
        &["chat", "--headless", "--prompt", "hello", "--json"],
        Some((
            "http://127.0.0.1:1/chat/completions",
            "fixture-model",
            "fixture-key",
        )),
    );
    assert_ne!(
        run.code(),
        0,
        "a corrupt configuration must not start a run"
    );
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    assert!(run.stderr.contains("config"), "{}", run.stderr);
    assert!(run.stderr.contains("config.toml"), "{}", run.stderr);
    assert!(
        run.stderr.contains("ha config validate"),
        "the error points at the command that explains it: {}",
        run.stderr
    );
    // The rejected value is never echoed back into the terminal.
    assert!(!run.stderr.contains("not-a-number"), "{}", run.stderr);
}

#[test]
fn i09_an_invalid_project_directory_stops_the_run_with_an_actionable_error() {
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let missing = project.join("does-not-exist");

    let run = run_headless_raw(
        &sandbox,
        &project,
        &[
            "chat",
            "--headless",
            "--cwd",
            missing.to_str().expect("utf-8 path"),
            "--prompt",
            "hello",
            "--json",
        ],
        Some((
            "http://127.0.0.1:1/chat/completions",
            "fixture-model",
            "fixture-key",
        )),
    );
    assert_ne!(run.code(), 0, "a missing project must not start a run");
    assert!(run.stderr.contains("does-not-exist"), "{}", run.stderr);
    assert!(
        run.stderr.contains("cannot be opened"),
        "the message says what failed: {}",
        run.stderr
    );
}

#[test]
fn i16_a_second_run_in_the_same_project_is_refused_while_the_first_holds_the_store() {
    let (endpoint, accepted, hold) = hanging_endpoint();
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let mut first = std::process::Command::new(cli_binary())
        .args([
            "chat",
            "--headless",
            "--prompt",
            "hold the store open",
            "--json",
        ])
        .current_dir(&project)
        .env("HA_HOME", sandbox.path())
        .env("HA_PROVIDER_ENDPOINT", &endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .expect("first run starts");

    // Wait until the first run really owns the project store: it only reaches the
    // provider after the writer was opened.
    let deadline = Instant::now() + Duration::from_secs(30);
    while !accepted.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < deadline,
            "the first run never reached the provider"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let second = run_headless_raw(
        &sandbox,
        &project,
        &["chat", "--headless", "--prompt", "second writer", "--json"],
        Some((&endpoint, "fixture-model", "fixture-secret-value")),
    );

    let _ = first.kill();
    let _ = first.wait();
    let _ = hold.join();

    assert_ne!(second.code(), 0, "the second writer must be refused");
    assert!(second.stdout.is_empty(), "stdout was: {}", second.stdout);
    assert!(
        second.stderr.contains("another writable host"),
        "the refusal names the real owner conflict: {}",
        second.stderr
    );
    assert!(
        second.stderr.contains("writer_locked"),
        "the refusal keeps its typed code: {}",
        second.stderr
    );
}

/// A provider endpoint that accepts one connection, reports that it did, and then
/// never answers, so the caller keeps the turn (and the store writer) open.
fn hanging_endpoint() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("hanging listener");
    let address = listener.local_addr().expect("hanging address");
    let accepted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&accepted);
    let handle = std::thread::spawn(move || {
        if let Ok((connection, _)) = listener.accept() {
            flag.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(30));
            drop(connection);
        }
    });
    (
        format!("http://{address}/chat/completions"),
        accepted,
        handle,
    )
}

// ---------------------------------------------------------------------------
// H05 I13 - a hard kill of the real process in the middle of a turn
// ---------------------------------------------------------------------------

/// A provider that accepts the request and then never answers. The stop channel
/// releases the socket, so the test never waits out a long hold.
fn stalling_endpoint() -> (
    String,
    Arc<AtomicBool>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("stalling listener");
    let address = listener.local_addr().expect("stalling address");
    let accepted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&accepted);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        if let Ok((connection, _)) = listener.accept() {
            flag.store(true, Ordering::SeqCst);
            // Hold the socket open without answering until the test releases it.
            let _ = held.recv_timeout(Duration::from_mins(1));
            drop(connection);
        }
    });
    (
        format!("http://{address}/chat/completions"),
        accepted,
        release,
        handle,
    )
}

/// The single per-project store the app created under `HA_HOME`.
fn only_project_store(sandbox: &Sandbox) -> PathBuf {
    let projects = sandbox.path().join("data").join("projects");
    let mut stores: Vec<PathBuf> = std::fs::read_dir(&projects)
        .expect("the app created a per-project store")
        .map(|entry| entry.expect("project store entry").path())
        .collect();
    stores.sort();
    assert_eq!(stores.len(), 1, "one project store is expected: {stores:?}");
    stores.pop().expect("one store")
}

/// Read-only operator view: durable sessions, no runtime.
fn session_list(sandbox: &Sandbox, store: &Path) -> serde_json::Value {
    let store = store.to_string_lossy().into_owned();
    let run = sandbox.run(&["sessions", "list", "--data-dir", store.as_str(), "--json"]);
    assert_eq!(run.code(), 0, "session list failed: {}", run.stderr);
    serde_json::from_str(&run.stdout).expect("session list prints JSON")
}

/// Read-only recovery view of one session, which is where a claimed success
/// would have to show up as a receipt.
fn session_status(sandbox: &Sandbox, store: &Path, session: &str) -> serde_json::Value {
    let store = store.to_string_lossy().into_owned();
    let run = sandbox.run(&[
        "status",
        "--data-dir",
        store.as_str(),
        "--session-id",
        session,
        "--json",
    ]);
    assert_eq!(run.code(), 0, "status failed: {}", run.stderr);
    serde_json::from_str(&run.stdout).expect("status prints JSON")
}

#[test]
fn i13_a_hard_kill_mid_turn_leaves_one_admitted_input_and_no_claimed_success() {
    hard_kill_scenario()
        .unwrap_or_else(|stderr| panic!("the killed run never reached the provider: {stderr}"));
}

/// One kill-then-inspect run in its own sandbox. `Err` carries the child stderr
/// when the process ended before the provider ever saw it; every durable
/// invariant is asserted here, where a failure is a real failure.
#[allow(clippy::too_many_lines)] // One kill-then-inspect sequence; splitting hides the order.
fn hard_kill_scenario() -> Result<(), String> {
    let (endpoint, accepted, release, hold) = stalling_endpoint();
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");

    let mut doomed = std::process::Command::new(cli_binary())
        .args([
            "chat",
            "--headless",
            "--prompt",
            "patch the parser",
            "--json",
        ])
        .current_dir(&project)
        .env("HA_HOME", sandbox.path())
        .env("HA_PROVIDER_ENDPOINT", &endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the doomed run starts");

    // The turn must be in flight before the kill: the provider only sees the
    // request after the input was admitted and the writer was taken.
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        if accepted.load(Ordering::SeqCst) {
            break;
        }
        if doomed.try_wait().expect("wait on the doomed run").is_some() {
            let mut stderr = String::new();
            if let Some(mut pipe) = doomed.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let _ = release.send(());
            hold.join().expect("the stalling endpoint ends");
            return Err(stderr);
        }
        assert!(
            Instant::now() < deadline,
            "the doomed run never reached the provider"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // A hard kill: the process dies with no unwinding, no flush and no graceful
    // writer shutdown. This is the case a simulated drop cannot prove.
    doomed.kill().expect("the run is killed");
    let status = doomed.wait().expect("the killed run is reaped");
    assert!(!status.success(), "a killed run must not report success");
    let _ = release.send(());
    hold.join().expect("the stalling endpoint ends");

    // What a new host finds, read the way an operator reads it: no runtime.
    let store = only_project_store(&sandbox);
    let listed = session_list(&sandbox, &store);
    let sessions = listed["sessions"]
        .as_array()
        .expect("sessions is an array")
        .clone();
    assert_eq!(
        sessions.len(),
        1,
        "exactly one session is durable: {listed}"
    );
    let session_id = sessions[0]["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();
    assert_eq!(
        sessions[0]["input_count"],
        serde_json::json!(1),
        "the interrupted input was admitted exactly once: {listed}"
    );
    assert_eq!(
        sessions[0]["next_sequence"],
        serde_json::json!(2),
        "the session consumed the sequence, so it cannot admit that input again: {listed}"
    );
    let recovered = session_status(&sandbox, &store, &session_id);
    assert_eq!(
        recovered["recovery"]["receipt_count"],
        serde_json::json!(0),
        "a killed turn claims no success: {recovered}"
    );
    assert_eq!(
        recovered["recovery"]["replayed_through_sequence"],
        serde_json::json!(1),
        "replay stops at the admitted input, so nothing was recorded beyond it: {recovered}"
    );
    assert_eq!(
        recovered["latest_snapshot_sequence"],
        serde_json::Value::Null,
        "the killed turn left no snapshot: {recovered}"
    );

    // A new host takes the store over and answers a second request in a new
    // session. The killed session keeps its single admitted input.
    let follow_up = follow_up_turn(&sandbox, &project).expect("the new host answers");
    let parsed: serde_json::Value =
        serde_json::from_str(&follow_up.stdout).expect("headless output is JSON");
    let new_session = parsed["session_id"].as_str().expect("a session id");
    assert_ne!(
        new_session, session_id,
        "the follow-up is a new session, not a replay of the killed input"
    );
    assert_eq!(
        parsed["stop"],
        serde_json::json!("final"),
        "the new host finishes its own turn: {parsed}"
    );
    assert_eq!(
        parsed["resumed_from"],
        serde_json::Value::Null,
        "the follow-up does not claim to have resumed the killed input: {parsed}"
    );
    // Positive control for the assertions above: a turn that did answer moves
    // replay one sequence further, so "replayed through 1" above really means
    // "nothing beyond the input was recorded".
    let new_status = session_status(&sandbox, &store, new_session);
    assert_eq!(
        new_status["recovery"]["replayed_through_sequence"],
        serde_json::json!(2),
        "a completed turn records one more sequence than its input: {new_status}"
    );
    assert_eq!(
        new_status["next_sequence"],
        serde_json::json!(3),
        "the completed session advanced past its answer: {new_status}"
    );
    let after = session_list(&sandbox, &store);
    let killed = after["sessions"]
        .as_array()
        .expect("sessions is an array")
        .iter()
        .find(|entry| entry["session_id"] == serde_json::json!(session_id))
        .cloned()
        .expect("the killed session is still listed");
    assert_eq!(
        killed["input_count"],
        serde_json::json!(1),
        "the follow-up did not admit anything into the killed session: {after}"
    );
    let still = session_status(&sandbox, &store, &session_id);
    assert_eq!(
        still["recovery"]["receipt_count"],
        serde_json::json!(0),
        "the killed turn is still unclaimed after the new host ran: {still}"
    );
    Ok(())
}

/// One successful headless turn with a fresh one-shot fixture.
fn follow_up_turn(sandbox: &Sandbox, project: &Path) -> Result<CliRun, String> {
    let (endpoint, server) = sse_fixture("handled in a new session");
    let run = run_headless_raw(
        sandbox,
        project,
        &[
            "chat",
            "--headless",
            "--prompt",
            "continue after the kill",
            "--json",
        ],
        Some((&endpoint, "fixture-model", "fixture-secret-value")),
    );
    let _ = server.join().expect("fixture server finishes");
    if run.code() == 0 {
        Ok(run)
    } else {
        Err(run.stderr)
    }
}

// ---------------------------------------------------------------------------
// I04 / I09 - an installed copy from a Unicode path, and an unusable data root
// ---------------------------------------------------------------------------

#[test]
fn i04_the_binary_installed_under_a_unicode_path_follows_the_caller_directory() {
    let sandbox = Sandbox::new();
    // An installed copy outside the build tree, under a path with spaces and
    // Vietnamese characters: this is the artifact an end user runs, not the
    // cargo output path.
    let install = sandbox.path().join("bản cài đặt");
    std::fs::create_dir_all(&install).expect("install dir");
    let installed = install.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(cli_binary(), &installed).expect("the artifact is installed");

    for name in ["dự án một", "dự án hai"] {
        let caller = sandbox.path().join(name);
        std::fs::create_dir_all(&caller).expect("caller dir");
        assert!(
            !caller.join(".git").exists(),
            "the caller directory is not a Git repository"
        );

        let (endpoint, server) = sse_fixture("the installed binary answers");
        let output = std::process::Command::new(&installed)
            .args(["chat", "--headless", "--prompt", "hello", "--json"])
            .current_dir(&caller)
            .env("HA_HOME", sandbox.path())
            .env("HA_PROVIDER_ENDPOINT", &endpoint)
            .env("HA_PROVIDER_MODEL", "fixture-model")
            .env("DEEPSEEK_API_KEY", "fixture-secret-value")
            .stdin(Stdio::null())
            .output()
            .expect("the installed binary runs");
        let run = CliRun::from_output(&output);
        let _ = server.join().expect("fixture server finishes");
        assert_eq!(
            run.code(),
            0,
            "the installed binary completes a turn from {}: {}",
            caller.display(),
            run.stderr
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&run.stdout).expect("headless output is JSON");
        assert_eq!(
            parsed["response"],
            serde_json::json!("the installed binary answers")
        );
        assert_eq!(parsed["fixture"], serde_json::json!(false));
    }

    // The project store follows the caller directory - not the install path and
    // not the build tree - so two caller directories own two stores.
    let projects = sandbox.path().join("data").join("projects");
    let mut stores: Vec<String> = std::fs::read_dir(&projects)
        .expect("the app created per-project stores")
        .map(|entry| {
            entry
                .expect("store entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    stores.sort();
    assert_eq!(
        stores.len(),
        2,
        "one store per caller directory: {stores:?}"
    );
    let installed_files: Vec<String> = std::fs::read_dir(&install)
        .expect("install dir readable")
        .map(|entry| {
            entry
                .expect("install entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        installed_files.len(),
        1,
        "the install directory only holds the binary: {installed_files:?}"
    );
}

#[test]
fn i09_a_data_root_that_cannot_be_created_names_the_path_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    // `HA_HOME` is a regular file here, so `<HA_HOME>/data` cannot be created.
    // The run must stop with an actionable error instead of silently falling back
    // to another location or starting without durable state.
    let home = sandbox.path().join("home-is-a-file");
    std::fs::write(&home, "not a directory").expect("fixture file");

    let mut command = std::process::Command::new(cli_binary());
    let run = CliRun::from_output(
        &command
            .args(["chat", "--headless", "--prompt", "hello", "--json"])
            .current_dir(&project)
            .env("HA_HOME", &home)
            .env(
                "HA_PROVIDER_ENDPOINT",
                "http://127.0.0.1:1/chat/completions",
            )
            .env("HA_PROVIDER_MODEL", "fixture-model")
            .env("DEEPSEEK_API_KEY", "fixture-secret-value")
            .stdin(Stdio::null())
            .output()
            .expect("ha binary runs"),
    );

    assert_ne!(run.code(), 0, "an unusable data root must not start a run");
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    // Which layer reports the unusable root first is platform-dependent, and the
    // test asserts the contract rather than one platform's ordering: on Windows the
    // store open fails and says so, while on Linux the config file *inside* the
    // unusable root is read first and fails with `config_read_error`. Both are the
    // same actionable failure, and both must name the path.
    assert!(
        run.stderr.contains("config_read_error") || run.stderr.contains("storage_open_failed"),
        "the failure carries a typed code: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("home-is-a-file"),
        "the failure names the unusable root: {}",
        run.stderr
    );
    assert!(home.is_file(), "the data root was not replaced by defaults");
    assert!(
        !sandbox.path().join("data").exists(),
        "no state was created next to the unusable root"
    );
}

/// A real permission denial on the data root, restored when the guard drops so a
/// failing assertion cannot leave an undeletable temporary directory behind.
struct DeniedWrite {
    directory: PathBuf,
    identity: String,
}

impl DeniedWrite {
    fn apply(directory: &Path) -> Self {
        // The process token is authoritative. Sandboxes and service hosts can
        // preserve USERNAME from the interactive account while running the test
        // under a different restricted identity.
        let current = std::process::Command::new("whoami")
            .output()
            .expect("whoami runs");
        assert!(
            current.status.success(),
            "whoami resolves the test identity"
        );
        let identity = String::from_utf8(current.stdout)
            .expect("whoami prints UTF-8")
            .trim()
            .to_owned();
        assert!(!identity.is_empty(), "whoami prints a non-empty identity");
        let output = std::process::Command::new("icacls")
            .arg(directory)
            .arg("/deny")
            // Full deny includes AddSubdirectory/CreateFiles. The basic `(W)`
            // mask is not sufficient on every Windows ACL inherited by Temp.
            .arg(format!("{identity}:(OI)(CI)(F)"))
            .output()
            .expect("icacls runs");
        assert!(
            output.status.success(),
            "icacls could not deny write on {}: {}",
            directory.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        Self {
            directory: directory.to_owned(),
            identity,
        }
    }
}

impl Drop for DeniedWrite {
    fn drop(&mut self) {
        let _ = std::process::Command::new("icacls")
            .arg(&self.directory)
            .arg("/remove:d")
            .arg(&self.identity)
            .output();
    }
}

#[test]
fn i09_a_data_directory_without_write_permission_names_the_path_and_writes_nothing() {
    // I09 names a data directory permission failure. This denies the current user
    // write access on the data root with a real ACL, so the failure comes from the
    // file system rather than from a fixture.
    //
    // The denial is applied with `icacls` and the process identity comes from
    // `whoami`, so this case is Windows-only by construction. It is skipped
    // rather than failed on Unix: the behaviour it proves is still covered there by
    // the read-only and unusable-root cases, and a skipped test that says why is
    // more honest than one that pretends the platform is unsupported.
    if !cfg!(windows) {
        eprintln!(
            "i09: write-denial fixture needs icacls and whoami; skipping on {}",
            std::env::consts::OS
        );
        return;
    }
    let sandbox = Sandbox::new();
    let project = sandbox.path().join("project");
    std::fs::create_dir_all(&project).expect("project dir");
    let home = sandbox.path().join("home");
    let data = home.join("data");
    std::fs::create_dir_all(&data).expect("data root");
    let denied = DeniedWrite::apply(&data);
    assert!(
        std::fs::create_dir(data.join("probe")).is_err(),
        "the ACL denial is effective, so the run below really meets a permission error"
    );

    let run = CliRun::from_output(
        &std::process::Command::new(cli_binary())
            .args(["chat", "--headless", "--prompt", "hello", "--json"])
            .current_dir(&project)
            .env("HA_HOME", &home)
            .env(
                "HA_PROVIDER_ENDPOINT",
                "http://127.0.0.1:1/chat/completions",
            )
            .env("HA_PROVIDER_MODEL", "fixture-model")
            .env("DEEPSEEK_API_KEY", "fixture-secret-value")
            .stdin(Stdio::null())
            .output()
            .expect("ha binary runs"),
    );

    assert_ne!(run.code(), 0, "a denied data root must not start a run");
    assert!(run.stdout.is_empty(), "stdout was: {}", run.stdout);
    assert!(
        run.stderr.contains("cannot open the project store at"),
        "the failure says what could not be opened: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("home"),
        "the failure names the resolved path: {}",
        run.stderr
    );
    assert!(
        run.stderr.contains("storage_open_failed"),
        "the typed code survives the extra context: {}",
        run.stderr
    );
    assert!(
        !data.join("projects").exists(),
        "no project store was created inside the denied root"
    );
    drop(denied);
    std::fs::create_dir(data.join("probe")).expect("the denial is removed again");
}
