//! `HA_LAUNCH` H07 PTY acceptance: the interactive app in a real terminal.
//!
//! This file is the only place that proves the no-arg launch owns a terminal:
//! `Command::output()` creates pipes, which is exactly the non-interactive case.
//! Every test below spawns the compiled binary inside a real pseudo-console and
//! reads the transcript it produces.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Resolve the compiled `ha` executable this crate produced.
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

/// One interactive session inside a real pseudo-console.
struct PtySession {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    /// All input goes through one queue: the test's keystrokes and the terminal
    /// emulator's replies must never contend for a lock, and a slow write must not
    /// block the reader that has to keep draining the console.
    input: std::sync::mpsc::Sender<Vec<u8>>,
    transcript: Arc<Mutex<Vec<u8>>>,
    /// The master handle must outlive the session: dropping it closes the
    /// pseudo-console, which silently stops output and leaves the child blocked.
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

/// Terminal emulator duties the harness must perform.
///
/// `ConPTY` asks the host terminal for the cursor position with ESC[6n once the app
/// switches its console into virtual-terminal input mode, and the app blocks until
/// the report arrives. A real terminal answers; a bare pipe does not, which is why
/// this reply is required for any transcript to appear at all.
const CURSOR_POSITION_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_POSITION_REPORT: &[u8] = b"\x1b[1;1R";

impl PtySession {
    fn spawn(cwd: &Path, env: &[(&str, String)]) -> Self {
        Self::spawn_executable(&cli_binary(), cwd, env, &[])
    }

    /// Spawn any `ha` executable: the build-tree binary by default, an installed
    /// artifact when a test must prove the copy a user actually gets.
    ///
    /// `remove` names extra inherited variables to drop, so a case can rebuild the
    /// environment instead of measuring the developer's shell.
    fn spawn_executable(
        binary: &Path,
        cwd: &Path,
        env: &[(&str, String)],
        remove: &[&str],
    ) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pseudo-console can be opened");
        let mut command = CommandBuilder::new(binary);
        command.cwd(cwd);
        // Never inherit a credential from the developer's shell: the tests decide
        // whether the app is configured. This runs before the explicit environment
        // so a test that wants a credential can still set one.
        command.env_remove("DEEPSEEK_API_KEY");
        command.env_remove("HA_API_KEY");
        command.env_remove("HA_PROVIDER_ENDPOINT");
        command.env_remove("HA_PROVIDER_MODEL");
        for name in remove {
            command.env_remove(*name);
        }
        for (name, value) in env {
            command.env(name, value);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .expect("the app starts inside the pseudo-console");
        drop(pair.slave);
        let master = pair.master;
        let mut reader = master
            .try_clone_reader()
            .expect("the master side is readable");
        let (input, queue) = std::sync::mpsc::channel::<Vec<u8>>();
        let mut writer = master.take_writer().expect("the master side is writable");
        std::thread::spawn(move || {
            while let Ok(bytes) = queue.recv() {
                if writer.write_all(&bytes).is_err() {
                    return;
                }
                if writer.flush().is_err() {
                    return;
                }
            }
        });
        let transcript = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&transcript);
        let answer_input = input.clone();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = reader.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                if let Ok(mut buffer) = sink.lock() {
                    buffer.extend_from_slice(bytes);
                }
                if bytes
                    .windows(CURSOR_POSITION_QUERY.len())
                    .any(|window| window == CURSOR_POSITION_QUERY)
                {
                    // The queue is unbounded, so the reader never blocks here: it
                    // must keep draining the console even when the app is not
                    // reading input yet.
                    let _ = answer_input.send(CURSOR_POSITION_REPORT.to_vec());
                }
            }
        });
        Self {
            child,
            input,
            transcript,
            _master: master,
        }
    }

    fn send(&mut self, text: &str) {
        self.input
            .send(text.as_bytes().to_vec())
            .expect("the writer thread is alive");
    }

    fn transcript(&self) -> String {
        let buffer = self.transcript.lock().expect("transcript lock").clone();
        String::from_utf8_lossy(&buffer).into_owned()
    }

    /// Wait until the transcript contains the needle, returning what was seen.
    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let text = self.transcript();
            if text.contains(needle) {
                return text;
            }
            assert!(
                Instant::now() <= deadline,
                "timed out waiting for {needle:?}; transcript was:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Wait until any of the needles appears; None when the deadline passes.
    ///
    /// Used where the console decides what the user's input turns into, so the test
    /// can assert the app's behaviour instead of the terminal's.
    fn wait_for_any(&self, needles: &[&str], timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            let text = self.transcript();
            if needles.iter().any(|needle| text.contains(needle)) {
                return Some(text);
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_exit(&mut self, timeout: Duration) -> Option<u32> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.exit_code().into(),
                Ok(None) => {}
                Err(error) => panic!("waiting for the app failed: {error}"),
            }
            if Instant::now() > deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// Isolated state root for one session.
fn sandbox() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("temp root");
    let project = temp.path().join("project with spaces");
    std::fs::create_dir_all(&project).expect("project dir");
    (temp, project)
}

fn base_env(temp: &tempfile::TempDir) -> Vec<(&'static str, String)> {
    vec![
        (
            "HA_HOME",
            temp.path().join("home").to_string_lossy().into_owned(),
        ),
        ("HA_PROVIDER_ENDPOINT", String::new()),
        ("HA_PROVIDER_MODEL", String::new()),
    ]
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i01_bare_launch_opens_the_app_in_a_real_terminal_and_exits_cleanly() {
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));

    let text = session.wait_for("Harness Agents", Duration::from_secs(30));
    assert!(session.is_alive(), "the app stays alive at the prompt");
    assert!(
        text.contains("Nhập yêu cầu"),
        "the prompt is Vietnamese: {text}"
    );
    assert!(
        text.contains(&project.display().to_string()),
        "the header names the caller project: {text}"
    );
    assert!(
        text.contains("setup required"),
        "an unconfigured provider is stated, not hidden: {text}"
    );

    session.send("/exit\r");
    let status = session.wait_exit(Duration::from_secs(20));
    assert_eq!(status, Some(0), "transcript:\n{}", session.transcript());
    // The shell must get its prompt back: the app leaves the cursor on a fresh
    // line instead of parking it inside its own prompt.
    let transcript = session.transcript();
    assert!(
        transcript.ends_with("\r\n") || transcript.ends_with('\n'),
        "the app restores the terminal before exiting: {transcript:?}"
    );
}

/// I14 (interactive half): the artifact a user installs, not the build-tree binary.
///
/// The installer self test proves the installed digest matches the built artifact;
/// this case proves the installed copy itself opens the app. It is staged under a
/// path with spaces and Vietnamese diacritics, started from a project directory
/// that is neither the install directory nor a Git repository, with PATH and the
/// profile variables rebuilt so no toolchain or developer state is reachable.
#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i14_the_installed_artifact_opens_the_app_in_a_real_terminal() {
    let (temp, project) = sandbox();
    let install = temp.path().join("bản cài đặt");
    std::fs::create_dir_all(&install).expect("install directory");
    let installed = install.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(cli_binary(), &installed)
        .expect("the installed copy comes from the built artifact");
    assert!(
        installed.is_file(),
        "installed artifact at {}",
        installed.display()
    );

    // A rebuilt environment: the install directory is the only place on PATH that
    // could resolve the command, and the developer profile and toolchain variables
    // are gone, so nothing here can fall back to the machine that built it.
    let system_root = std::env::var("SystemRoot").expect("Windows sets SystemRoot");
    let mut env = base_env(&temp);
    env.push((
        "PATH",
        format!(
            "{};{system_root}\\System32;{system_root}",
            install.display()
        ),
    ));
    env.push(("SystemRoot", system_root.clone()));
    env.push((
        "SystemDrive",
        system_root.chars().take(2).collect::<String>(),
    ));
    let remove = [
        "APPDATA",
        "LOCALAPPDATA",
        "USERPROFILE",
        "HOME",
        "CARGO_HOME",
        "RUSTUP_HOME",
    ];

    let mut session = PtySession::spawn_executable(&installed, &project, &env, &remove);
    let text = session.wait_for("Harness Agents", Duration::from_secs(30));
    assert!(
        session.is_alive(),
        "the installed app stays alive at the prompt"
    );
    assert!(
        text.contains("Nhập yêu cầu"),
        "the prompt is Vietnamese: {text}"
    );
    assert!(
        text.contains(&format!("Project: {}", project.display())),
        "the header names the caller project: {text}"
    );
    // The state root is the caller's HA_HOME, not anything next to the binary. The
    // store itself is created on the first request, so boot only resolves the path.
    let data_root = ha_home(&temp).join("data");
    assert!(
        text.contains(&format!("Data:    {} [HA_HOME]", data_root.display())),
        "the header reports the caller's data root with its origin: {text}"
    );
    assert!(
        text.contains(&format!(
            "Store:   {}",
            data_root.join("projects").display()
        )),
        "the store is scoped to the caller project under HA_HOME: {text}"
    );

    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
    let transcript = session.transcript();
    assert!(
        transcript.ends_with("\r\n") || transcript.ends_with('\n'),
        "the installed app restores the terminal before exiting: {transcript:?}"
    );
    // Read the install directory the way an operator would: it must still hold the
    // artifact and nothing the app wrote while it ran.
    let mut staged: Vec<String> = std::fs::read_dir(&install)
        .expect("the install directory is readable")
        .map(|entry| {
            entry
                .expect("install entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    staged.sort();
    assert_eq!(
        staged.len(),
        1,
        "the install directory holds only the artifact: {staged:?}"
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i06_pty_keeps_vietnamese_input_and_paste_intact() {
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Harness Agents", Duration::from_secs(30));

    session.send("sửa lỗi parser");
    session.wait_for("sửa lỗi parser", Duration::from_secs(15));
    session.send("\u{7f}");
    session.send("!");
    session.wait_for("sửa lỗi parse!", Duration::from_secs(15));
    assert!(session.is_alive(), "editing keeps the app alive");

    // A paste must never turn into several submitted commands. Whether the console
    // forwards the bracketed-paste markers is the console's choice: this ConPTY
    // build does not, and then the newline inside the paste arrives as Enter. Both
    // outcomes are asserted, so the test measures the app instead of the terminal.
    session.send("\u{1b}[200~multi\r\nline\u{1b}[201~");
    let pasted = session.wait_for_any(&["multi line", "multi"], Duration::from_secs(15));
    let transcript = session.transcript();
    let Some(_) = pasted else {
        panic!("the pasted text never reached the prompt:\n{transcript}");
    };
    if transcript.contains("multi line") {
        assert!(
            !transcript.contains("[run] accepted"),
            "a bracketed paste must not submit a request:\n{transcript}"
        );
    } else {
        // Measured on this ConPTY: the bracketed-paste markers are not forwarded, so
        // the newline inside the paste arrives as Enter and the first part is
        // submitted as one request. Nothing can fix that in the app; what the app
        // must do is stay usable and keep accepting input afterwards.
        eprintln!("i06: console without bracketed paste; asserting the prompt survives");
        assert!(
            session.is_alive(),
            "the app survives a console without bracketed paste:\n{transcript}"
        );
        session.send("\u{3}");
        session.send("ok");
        session.wait_for("> ok", Duration::from_secs(15));
        assert!(
            session.is_alive(),
            "the prompt is still usable after a paste"
        );
    }

    // Clear the prompt before the command: /exit is only a command when the line
    // starts with it, so a leftover character would turn it into a request.
    session.send("\u{3}");
    session.wait_for_any(&["> "], Duration::from_secs(10));
    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i07a_ctrl_c_clears_an_idle_prompt() {
    // One pseudo-console per test: opening a second one in the same process blocks
    // on this host, so the idle and running phases cannot share one test.
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Harness Agents", Duration::from_secs(30));

    session.send("typo");
    session.wait_for("> typo", Duration::from_secs(15));
    session.send("\u{3}");
    session.send("z");
    session.wait_for("> z", Duration::from_secs(15));
    assert!(
        !session.transcript().contains("> typoz"),
        "an idle Ctrl-C clears the buffer:\n{}",
        session.transcript()
    );
    assert!(session.is_alive());

    // Clear the marker character first: "/exit" appended to it would be submitted
    // as a request instead of a command.
    session.send("\u{3}");
    session.wait_for_any(&["> "], Duration::from_secs(10));
    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i07b_ctrl_c_cancels_a_running_turn() {
    let (temp, project) = sandbox();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("hanging endpoint");
    listener
        .set_nonblocking(true)
        .expect("the fixture listener is non-blocking");
    let address = listener.local_addr().expect("hanging address");
    // A blocking accept() here would hang the test itself whenever the app never
    // reaches the provider, which is exactly what this case must be able to report.
    let contacted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&contacted);
    let hold = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_mins(1);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((connection, _)) => {
                    flag.store(true, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_secs(8));
                    drop(connection);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    let mut running_env = base_env(&temp);
    running_env.push((
        "HA_PROVIDER_ENDPOINT",
        format!("http://{address}/chat/completions"),
    ));
    running_env.push(("HA_PROVIDER_MODEL", "fixture-model".to_owned()));
    running_env.push(("DEEPSEEK_API_KEY", "fixture-secret-value".to_owned()));
    let mut running = PtySession::spawn(&project, &running_env);
    running.wait_for("Harness Agents", Duration::from_secs(30));

    running.send("hold the turn open\r");
    running.wait_for("[run] accepted", Duration::from_secs(20));
    let waiting = Instant::now() + Duration::from_secs(30);
    while !contacted.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < waiting,
            "the turn never reached the provider, so it is not waiting on it:\n{}",
            running.transcript()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    running.send("\u{3}");
    running.wait_for("canceling", Duration::from_secs(20));
    running.wait_for("[run]", Duration::from_secs(30));
    running.send("/exit\r");
    assert_eq!(
        running.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        running.transcript()
    );
    let text = running.transcript();
    assert!(
        !text.contains("fixture-secret-value"),
        "the credential never reaches the screen"
    );
    let _ = hold.join();
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i08_a_backend_fault_after_init_restores_the_terminal_and_is_not_swallowed() {
    // I08: inject a render/backend failure *after* the terminal is initialized and
    // raw mode is on. The fault is armed through the debug-only seam
    // HA_TEST_FAIL_AFTER_MS, which a release binary cannot be told to honour.
    let (temp, project) = sandbox();
    let mut env = base_env(&temp);
    env.push(("HA_TEST_FAIL_AFTER_MS", "1500".to_owned()));
    let mut session = PtySession::spawn(&project, &env);

    // The boot header and prompt are rendered before the deadline, so the failure
    // really happens after initialization rather than during startup.
    let booted = session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    assert!(booted.contains("Harness Agents"), "boot rendered: {booted}");
    assert!(session.is_alive(), "the app is at the prompt, in raw mode");
    std::thread::sleep(Duration::from_millis(1800));
    session.send("x");

    let status = session.wait_exit(Duration::from_secs(25));
    let transcript = session.transcript();
    assert_eq!(
        status,
        Some(1),
        "a terminal failure is a runtime error, not a hang and not a success:\n{transcript}"
    );
    assert!(
        transcript.contains("terminal input/output failed"),
        "the fatal error names itself instead of being swallowed:\n{transcript}"
    );
    assert!(
        transcript.contains("HA_TEST_FAIL_AFTER_MS"),
        "the reported failure is the injected one, not an unrelated error:\n{transcript}"
    );
}

// ---------------------------------------------------------------------------
// I13 - a settled tool receipt survives a hard kill of the real process
// ---------------------------------------------------------------------------

impl PtySession {
    /// Kill the app the way a crash does: no unwinding, no flush, no graceful close.
    fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

/// Environment for a turn that talks to a local fixture provider.
fn provider_env(temp: &tempfile::TempDir, endpoint: &str) -> Vec<(&'static str, String)> {
    let mut env = base_env(temp);
    env.push(("HA_PROVIDER_ENDPOINT", endpoint.to_owned()));
    env.push(("HA_PROVIDER_MODEL", "fixture-model".to_owned()));
    env.push(("DEEPSEEK_API_KEY", "fixture-secret-value".to_owned()));
    env
}

/// Run one non-PTY command against the same state root and parse its JSON stdout.
fn run_cli_json(temp: &tempfile::TempDir, project: &Path, args: &[&str]) -> serde_json::Value {
    let output = std::process::Command::new(cli_binary())
        .args(args)
        .current_dir(project)
        .env("HA_HOME", ha_home(temp))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("ha runs");
    assert!(
        output.status.success(),
        "ha {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("a JSON result")
}

/// The state root `base_env` hands to the app.
fn ha_home(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("home")
}

/// The single per-project store the app created under `HA_HOME`.
fn only_store(temp: &tempfile::TempDir) -> PathBuf {
    let projects = ha_home(temp).join("data").join("projects");
    let mut stores: Vec<PathBuf> = std::fs::read_dir(&projects)
        .expect("the app created a per-project store")
        .map(|entry| entry.expect("store entry").path())
        .collect();
    stores.sort();
    assert_eq!(stores.len(), 1, "one store is expected: {stores:?}");
    stores.pop().expect("one store")
}

/// Prove a loopback listener is reachable before a fresh process connects to it:
/// this sandbox occasionally refuses the first connection to a new port.
fn warm_up_loopback(endpoint: &str) {
    let authority = endpoint
        .strip_prefix("http://")
        .and_then(|rest| rest.split('/').next())
        .expect("the fixture endpoint is http");
    let deadline = Instant::now() + Duration::from_mins(1);
    loop {
        if std::net::TcpStream::connect(authority).is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the fixture listener never accepted a warm-up connection"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn read_request(socket: &mut std::net::TcpStream) -> String {
    let mut buffer = vec![0_u8; 8192];
    let read = socket.read(&mut buffer).expect("fixture reads");
    buffer.truncate(read);
    String::from_utf8_lossy(&buffer).into_owned()
}

fn write_sse(socket: &mut std::net::TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    socket
        .write_all(response.as_bytes())
        .expect("fixture writes");
    socket.flush().ok();
}

/// Provider script for the kill case: one gated patch request, a second call held
/// open (the window the test kills in), then a prose answer for the continuation.
fn patch_then_stall_endpoint(
    expected_hash: String,
    replacement: String,
) -> (
    String,
    Arc<AtomicBool>,
    Arc<AtomicBool>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let asked = Arc::new(AtomicBool::new(false));
    let continued = Arc::new(AtomicBool::new(false));
    let asked_flag = Arc::clone(&asked);
    let continued_flag = Arc::clone(&continued);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        let arguments = serde_json::json!({
            "path": "src/parser.rs",
            "expected_hash": expected_hash,
            "replacement": replacement,
        })
        .to_string();
        let tool_call = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"id\":\"patch-1\",\"function\":{{\"name\":\"apply_patch\",\"arguments\":{}}}}}]}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\ndata: [DONE]\n\n",
            serde_json::Value::String(arguments)
        );
        // 1. The turn asks for one gated patch.
        let (mut socket, _) = listener.accept().expect("the first call arrives");
        let _ = read_request(&mut socket);
        asked_flag.store(true, Ordering::SeqCst);
        write_sse(&mut socket, &tool_call);
        // 2. After the tool settles the app asks again: hold this call open so the
        //    test can kill the process with the receipt already committed.
        let (mut second, _) = listener.accept().expect("the second call arrives");
        let _ = read_request(&mut second);
        continued_flag.store(true, Ordering::SeqCst);
        let _ = held.recv_timeout(Duration::from_mins(2));
        drop(second);
        // 3. The continuation after the kill gets a prose answer. A warm-up connect
        //    that sends no request is not that call.
        loop {
            let (mut third, _) = listener.accept().expect("the continuation arrives");
            if read_request(&mut third).is_empty() {
                continue;
            }
            write_sse(
                &mut third,
                "data: {\"choices\":[{\"delta\":{\"content\":\"the parser is already fixed; nothing to redo\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            );
            break;
        }
    });
    (
        format!("http://{address}/chat/completions"),
        asked,
        continued,
        release,
        handle,
    )
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
#[allow(clippy::too_many_lines)] // One kill-then-resume sequence; splitting it hides the order.
fn i13_a_settled_tool_receipt_survives_a_hard_kill_mid_turn() {
    // I13: the kill lands *after* a granted tool action committed its receipt and
    // the turn already asked the model for the next step.
    let (temp, project) = sandbox();
    std::fs::create_dir_all(project.join("src")).expect("src dir");
    let original = "fn parse() { todo!() }\n";
    std::fs::write(project.join("src").join("parser.rs"), original).expect("fixture file");
    let expected = harness_types::ContentHash::from_bytes(original.as_bytes())
        .as_str()
        .to_owned();
    let replacement = "fn parse() { println!(\"fixed before the kill\"); }\n";
    let (endpoint, _asked, continued, release, server) =
        patch_then_stall_endpoint(expected, replacement.to_owned());

    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Harness Agents", Duration::from_secs(30));
    session.send("fix the parser\r");
    // The proposal is rendered with the contract action name, not the tool name.
    session.wait_for("[approval] ApplyPatch", Duration::from_secs(40));
    session.send("y\r");

    // The second model call only happens after the tool settled, so waiting for it
    // is what makes "killed with a committed receipt" a measurement, not a guess.
    let waiting = Instant::now() + Duration::from_mins(1);
    while !continued.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < waiting,
            "the turn never committed the tool and continued:\n{}",
            session.transcript()
        );
        assert!(
            session.is_alive(),
            "the app died before the kill:\n{}",
            session.transcript()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let patched = std::fs::read_to_string(project.join("src").join("parser.rs"))
        .expect("the granted patch wrote the file");
    assert!(
        patched.contains("fixed before the kill"),
        "the approved action really ran: {patched}"
    );

    // A hard kill: no unwinding, no flush, no graceful writer shutdown.
    session.kill();
    // Release the held call. The fixture thread then waits for the continuation,
    // so it must not be joined until that continuation has run.
    let _ = release.send(());

    // The durable state, read the way an operator reads it: no runtime.
    let store = only_store(&temp);
    let store = store.to_str().expect("the store path is UTF-8").to_owned();
    let listed = run_cli_json(
        &temp,
        &project,
        &["sessions", "list", "--data-dir", &store, "--json"],
    );
    let sessions = listed["sessions"].as_array().expect("sessions is an array");
    assert_eq!(
        sessions.len(),
        1,
        "exactly one session is durable: {listed}"
    );
    let session_id = sessions[0]["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();
    let status = run_cli_json(
        &temp,
        &project,
        &[
            "status",
            "--data-dir",
            &store,
            "--session-id",
            &session_id,
            "--json",
        ],
    );
    assert_eq!(
        status["recovery"]["receipt_count"],
        serde_json::json!(1),
        "the settled receipt survived the kill: {status}"
    );

    // A new process resumes the task. The settled action must not run again.
    warm_up_loopback(&endpoint);
    let resume = std::process::Command::new(cli_binary())
        .args([
            "chat",
            "--headless",
            "--resume",
            &session_id,
            "--prompt",
            "continue after the kill",
            "--json",
        ])
        .current_dir(&project)
        .env("HA_HOME", ha_home(&temp))
        .env("HA_PROVIDER_ENDPOINT", &endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the continuation runs");
    assert!(
        resume.status.success(),
        "resume failed: {}",
        String::from_utf8_lossy(&resume.stderr)
    );
    server.join().expect("the fixture server finishes");
    let parsed: serde_json::Value =
        serde_json::from_slice(&resume.stdout).expect("the continuation prints JSON");
    assert_eq!(
        parsed["tool_calls"],
        serde_json::json!(0),
        "the settled action is not re-executed: {parsed}"
    );
    let after = std::fs::read_to_string(project.join("src").join("parser.rs"))
        .expect("the file after the continuation");
    assert_eq!(after, patched, "the side effect happened exactly once");
    let still = run_cli_json(
        &temp,
        &project,
        &[
            "status",
            "--data-dir",
            &store,
            "--session-id",
            &session_id,
            "--json",
        ],
    );
    assert_eq!(
        still["recovery"]["receipt_count"],
        serde_json::json!(1),
        "no second receipt was written for the settled action: {still}"
    );
}

// ---------------------------------------------------------------------------
// I07 - /exit while a run is still active
// ---------------------------------------------------------------------------

/// Provider that accepts the request and never answers, with a stop channel so the
/// test does not wait out the hold.
fn stalling_provider() -> (
    String,
    Arc<AtomicBool>,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("stalling listener");
    listener
        .set_nonblocking(true)
        .expect("the stalling listener is non-blocking");
    let address = listener.local_addr().expect("stalling address");
    let contacted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&contacted);
    let (release, held) = std::sync::mpsc::channel::<()>();
    let handle = std::thread::spawn(move || {
        // A blocking accept() would hang the test whenever the app never reaches
        // the provider, which is exactly what this case must be able to report.
        let deadline = Instant::now() + Duration::from_mins(1);
        while Instant::now() < deadline {
            match listener.accept() {
                Ok((connection, _)) => {
                    flag.store(true, Ordering::SeqCst);
                    let _ = held.recv_timeout(Duration::from_mins(1));
                    drop(connection);
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => return,
            }
        }
    });
    (
        format!("http://{address}/chat/completions"),
        contacted,
        release,
        handle,
    )
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i05_exit_during_an_active_run_releases_the_store_for_the_next_host() {
    // H05: /exit must handle an active run (cancel, cleanup) and restore terminal
    // and store ownership. The proof for ownership is that the next host can write.
    let (temp, project) = sandbox();
    let (endpoint, contacted, release, server) = stalling_provider();
    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Harness Agents", Duration::from_secs(30));

    session.send("hold the turn open\r");
    session.wait_for("[run] accepted", Duration::from_secs(20));
    let waiting = Instant::now() + Duration::from_secs(30);
    while !contacted.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < waiting,
            "the turn never reached the provider:\n{}",
            session.transcript()
        );
        assert!(session.is_alive(), "the app is still running the turn");
        std::thread::sleep(Duration::from_millis(25));
    }

    // No Ctrl-C first: exit while the run is active.
    session.send("/exit\r");
    let status = session.wait_exit(Duration::from_secs(40));
    let transcript = session.transcript();
    assert_eq!(status, Some(0), "the app exits cleanly:\n{transcript}");
    assert!(
        transcript.contains("canceling the active run before exit"),
        "the exit says it canceled the active run:\n{transcript}"
    );
    let _ = release.send(());
    server.join().expect("the stalling provider ends");

    // Store ownership is restored: a new host can take the writer and write.
    let store = only_store(&temp);
    let store = store.to_str().expect("the store path is UTF-8").to_owned();
    let listed = run_cli_json(
        &temp,
        &project,
        &["sessions", "list", "--data-dir", &store, "--json"],
    );
    assert_eq!(
        listed["sessions"].as_array().expect("an array").len(),
        1,
        "the canceled run left one durable session: {listed}"
    );

    let (answer_endpoint, answer_server) = sse_answer("the store is free again");
    let follow_up = std::process::Command::new(cli_binary())
        .args(["chat", "--headless", "--prompt", "after the exit", "--json"])
        .current_dir(&project)
        .env("HA_HOME", ha_home(&temp))
        .env("HA_PROVIDER_ENDPOINT", &answer_endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the next host runs");
    answer_server.join().expect("the answer fixture finishes");
    assert!(
        follow_up.status.success(),
        "the next host took the store: {}",
        String::from_utf8_lossy(&follow_up.stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&follow_up.stdout).expect("the next host prints JSON");
    assert_eq!(
        parsed["response"],
        serde_json::json!("the store is free again"),
        "the next host completed its own turn: {parsed}"
    );
}

/// One-shot SSE provider that answers one request with plain text.
fn sse_answer(text: &str) -> (String, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("answer listener");
    let address = listener.local_addr().expect("answer address");
    let text = text.to_owned();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("the answer call arrives");
        let _ = read_request(&mut socket);
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        write_sse(&mut socket, &body);
    });
    (format!("http://{address}/chat/completions"), handle)
}

// ---------------------------------------------------------------------------
// I12 - a configured but unreachable provider (the offline path)
// ---------------------------------------------------------------------------

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all nine cases i01, i05, i06, i07a, i07b, i08, i12, i13 and i14 pass there."]
#[test]
fn i12_a_prompt_with_an_unreachable_provider_is_reported_and_the_app_stays_alive() {
    // The offline path: the environment is configured, but nothing answers. The app
    // must report the failure, stay usable, and never fabricate an answer.
    let (temp, project) = sandbox();
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe listener");
    let address = probe.local_addr().expect("probe address");
    drop(probe);
    let endpoint = format!("http://{address}/chat/completions");

    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Harness Agents", Duration::from_secs(30));
    session.send("hello\r");

    // A failed turn is rendered as a run failure naming the provider error.
    let transcript = session.wait_for("[run] failed", Duration::from_secs(40));
    assert!(
        transcript.contains(&address.to_string()),
        "the failure names the endpoint that was called:\n{transcript}"
    );
    assert!(
        session.is_alive(),
        "the app survives an unreachable provider:\n{transcript}"
    );
    assert!(
        !transcript.contains("fixture answer") && !transcript.contains("no model was called"),
        "an unreachable provider never turns into a fixture answer:\n{transcript}"
    );

    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}
