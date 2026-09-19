//! `HA_LAUNCH` H07 PTY acceptance: the interactive app in a real terminal.
//!
//! This file is the only place that proves the no-arg launch owns a terminal:
//! `Command::output()` creates pipes, which is exactly the non-interactive case.
//! Every test below spawns the compiled binary inside a real pseudo-console and
//! reads the transcript it produces.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
/// ConPTY asks the host terminal for the cursor position with ESC[6n once the app
/// switches its console into virtual-terminal input mode, and the app blocks until
/// the report arrives. A real terminal answers; a bare pipe does not, which is why
/// this reply is required for any transcript to appear at all.
const CURSOR_POSITION_QUERY: &[u8] = b"\x1b[6n";
const CURSOR_POSITION_REPORT: &[u8] = b"\x1b[1;1R";

impl PtySession {
    fn spawn(cwd: &Path, env: &[(&str, String)]) -> Self {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 110,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pseudo-console can be opened");
        let mut command = CommandBuilder::new(cli_binary());
        command.cwd(cwd);
        // Never inherit a credential from the developer's shell: the tests decide
        // whether the app is configured. This runs before the explicit environment
        // so a test that wants a credential can still set one.
        command.env_remove("DEEPSEEK_API_KEY");
        command.env_remove("HA_API_KEY");
        command.env_remove("HA_PROVIDER_ENDPOINT");
        command.env_remove("HA_PROVIDER_MODEL");
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console) or any terminal. State at H07: i01 passes there; i06 fails on the bracketed-paste expectation and i07 hangs the harness - both recorded in docs/evidence/HA_LAUNCH.vi.md."]
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console) or any terminal. State at H07: i01 passes there; i06 fails on the bracketed-paste expectation and i07 hangs the harness - both recorded in docs/evidence/HA_LAUNCH.vi.md."]
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

    // A bracketed paste arrives as one insertion; it must never become several
    // submitted lines.
    session.send("\u{1b}[200~multi\r\nline\u{1b}[201~");
    session.wait_for("multi line", Duration::from_secs(15));
    assert!(
        !session.transcript().contains("[run] accepted"),
        "a paste must not submit a request:\n{}",
        session.transcript()
    );
    assert!(session.is_alive());

    session.send("\r");
    session.wait_for("[run] accepted", Duration::from_secs(25));
    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console) or any terminal. State at H07: i01 passes there; i06 fails on the bracketed-paste expectation and i07 hangs the harness - both recorded in docs/evidence/HA_LAUNCH.vi.md."]
#[test]
fn i07_ctrl_c_clears_an_idle_prompt_and_cancels_a_running_turn() {
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Harness Agents", Duration::from_secs(30));

    // Idle: Ctrl-C clears whatever was typed.
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

    // Running: the provider accepts the connection and never answers, so the turn
    // stays active until Ctrl-C cancels it.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("hanging endpoint");
    let address = listener.local_addr().expect("hanging address");
    let hold = std::thread::spawn(move || {
        if let Ok((connection, _)) = listener.accept() {
            std::thread::sleep(Duration::from_secs(10));
            drop(connection);
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