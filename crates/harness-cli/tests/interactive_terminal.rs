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
    master: Box<dyn portable_pty::MasterPty + Send>,
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

    /// Spawn the build-tree binary with extra arguments and environment pairs.
    ///
    /// Used by cases that need a flag or a diagnostic variable the other cases do
    /// not set, without changing how every case launches the app. The T08 cases
    /// for the TUI use it as well.
    #[allow(dead_code, reason = "T08 adds TUI PTY cases that pass arguments")]
    fn spawn_process(
        cwd: &Path,
        env: &[(&str, String)],
        arguments: &[&str],
        extra_env: &[(&str, &str)],
    ) -> Self {
        let mut pairs: Vec<(&str, String)> = env.to_vec();
        for (name, value) in extra_env {
            pairs.push((name, (*value).to_owned()));
        }
        Self::spawn_command(&cli_binary(), cwd, &pairs, &[], arguments)
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
        Self::spawn_command(binary, cwd, env, remove, &[])
    }

    /// Spawn with explicit arguments.
    fn spawn_command(
        binary: &Path,
        cwd: &Path,
        env: &[(&str, String)],
        remove: &[&str],
        arguments: &[&str],
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
        for argument in arguments {
            command.arg(*argument);
        }
        // Never inherit a credential from the developer's shell: the tests decide
        // whether the app is configured. This runs before the explicit environment
        // so a test that wants a credential can still set one.
        command.env_remove("DEEPSEEK_API_KEY");
        command.env_remove("HA_API_KEY");
        command.env_remove("HA_PROVIDER_ENDPOINT");
        command.env_remove("HA_PROVIDER_MODEL");
        command.env_remove("HA_UI");
        command.env_remove("NO_COLOR");
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
            master,
        }
    }

    fn send(&mut self, text: &str) {
        self.await_console();
        self.input
            .send(text.as_bytes().to_vec())
            .expect("the writer thread is alive");
    }

    /// Wait until the child has painted something.
    ///
    /// A keystroke written before the app owns the console is lost on this
    /// `ConPTY`, which made the cases flaky; every case therefore types only after
    /// the first paint has arrived.
    fn await_console(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if !self.transcript().is_empty() {
                return;
            }
            assert!(Instant::now() <= deadline, "the app never painted anything");
            std::thread::sleep(Duration::from_millis(10));
        }
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

    fn resize(&mut self, columns: u16, rows: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("the pseudo-console resizes");
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // `portable_pty::Child::kill` is asynchronous on ConPTY. Reap the child
        // before the test binary exits, otherwise a failed case can keep ha.exe
        // locked and make the next cargo build fail with Access denied.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Keep the transcript of every case: when an assertion fails the panic
        // message carries the text, but the raw bytes are what a reader needs to
        // see which escape sequence landed where.
        if let Ok(mut buffer) = self.transcript.lock() {
            let name = std::env::var("HA_PTY_TRANSCRIPT_DIR")
                .unwrap_or_else(|_| "target/pty-transcripts".to_owned());
            let _ = std::fs::create_dir_all(&name);
            let thread = std::thread::current();
            let label = thread
                .name()
                .unwrap_or("pty")
                .rsplit("::")
                .next()
                .unwrap_or("pty")
                .to_owned();
            let _ = std::fs::write(format!("{name}/{label}.txt"), buffer.as_slice());
            buffer.clear();
        }
    }
}

/// The transcript with escape sequences removed and whitespace runs collapsed.
///
/// A PTY transcript is not a screen: the console's echo of the user's keystrokes
/// and the app's cursor moves arrive in whatever order `ConPTY` produced them, and a
/// per-keystroke repaint puts cursor moves inside a typed word. Assertions about
/// what the user could read therefore compare against this view - it keeps every
/// printable character in order - while assertions about the app's own output
/// (the echo lines, the run landmarks) use the raw transcript.
fn normalized(transcript: &str) -> String {
    let mut out = String::with_capacity(transcript.len());
    let mut characters = transcript.chars().peekable();
    let mut in_space = false;
    while let Some(character) = characters.next() {
        if character == '\u{1b}' {
            match characters.peek() {
                Some('[') => {
                    characters.next();
                    // CSI: consume until a final byte in @..~
                    for next in characters.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    characters.next();
                    // OSC: consume until BEL or ST
                    while let Some(next) = characters.next() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            let _ = characters.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if character.is_whitespace() {
            if !in_space {
                out.push(' ');
                in_space = true;
            }
            continue;
        }
        if character.is_control() {
            continue;
        }
        in_space = false;
        out.push(character);
    }
    out
}

/// Whether the transcript contains an SGR foreground/background colour.
/// Cursor movement and resets are allowed under `NO_COLOR`; palette selection is
/// not.
fn has_color_sgr(transcript: &str) -> bool {
    let bytes = transcript.as_bytes();
    let mut index = 0;
    while index + 2 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let start = index + 2;
        let Some(end_offset) = bytes[start..].iter().position(|byte| *byte == b'm') else {
            return false;
        };
        let end = start + end_offset;
        let parameters = String::from_utf8_lossy(&bytes[start..end]);
        if parameters
            .split(';')
            .filter_map(|part| part.parse::<u16>().ok())
            .any(|value| matches!(value, 30..=37 | 40..=47 | 90..=107 | 38 | 48))
        {
            return true;
        }
        index = end + 1;
    }
    false
}

/// Wait until the normalized transcript contains the needle.
fn wait_for_normalized(session: &PtySession, needle: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let seen = normalized(&session.transcript());
        if seen.contains(needle) {
            return seen;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {needle:?} in the normalized transcript; saw:\n{seen}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait until the needle has been printed `count` times.
///
/// A second occurrence is what proves a second turn ran: waiting for the same line again
/// would return the first one, which is already in the transcript.
fn wait_for_occurrences(
    session: &PtySession,
    needle: &str,
    count: usize,
    timeout: Duration,
) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let seen = normalized(&session.transcript());
        if seen.matches(needle).count() >= count {
            return seen;
        }
        assert!(
            Instant::now() <= deadline,
            "timed out waiting for {count} x {needle:?} in the normalized transcript; saw:\n{seen}"
        );
        std::thread::sleep(Duration::from_millis(25));
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
        // The Codex host can itself run with TERM=dumb. A real pseudo-console is
        // capable of cursor positioning, so the default acceptance route must
        // state that capability instead of silently measuring the plain fallback.
        ("TERM", "xterm-256color".to_owned()),
    ]
}

/// A loopback provider that asks for one `list_files` call, every time.
///
/// It is how a turn reaches a bound on purpose: the harness runs the tool, calls the
/// model again, and only the bound can end the loop.
struct ToolCallProvider {
    address: std::net::SocketAddr,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl ToolCallProvider {
    fn start() -> Self {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback provider");
        listener
            .set_nonblocking(true)
            .expect("the fixture listener is non-blocking");
        let address = listener.local_addr().expect("provider address");
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&requests);
        let finished = Arc::clone(&stop);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_mins(2);
            while Instant::now() < deadline && !finished.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut connection, _)) => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        let _ = read_http_request(&mut connection);
                        let body = tool_call_stream();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = connection.write_all(head.as_bytes());
                        let _ = connection.write_all(body.as_bytes());
                        let _ = connection.flush();
                        let _ = connection.shutdown(std::net::Shutdown::Both);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            address,
            requests,
            stop,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}/chat/completions", self.address)
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for ToolCallProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Read one HTTP request off the socket, so the reply is never written mid-request.
fn read_http_request(connection: &mut std::net::TcpStream) -> std::io::Result<String> {
    connection.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    let header_end = loop {
        let read = connection.read(&mut chunk)?;
        if read == 0 {
            return Ok(String::from_utf8_lossy(&buffer).into_owned());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())?
        })
        .unwrap_or(0);
    while buffer.len() < header_end + length {
        let read = connection.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// One streamed `list_files` call, in the shape the decoder reads.
fn tool_call_stream() -> String {
    let frame = |delta: &str, finish: &str| {
        format!("data: {{\"choices\":[{{\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n")
    };
    [
        frame("{\"role\":\"assistant\",\"content\":\"\"}", "null"),
        frame(
            "{\"tool_calls\":[{\"index\":0,\"id\":\"call_bound_1\",\"type\":\"function\",\
             \"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"path\\\": \\\".\\\"}\"}}]}",
            "null",
        ),
        frame("{}", "\"tool_calls\""),
        "data: [DONE]\n\n".to_owned(),
    ]
    .concat()
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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
        transcript.ends_with("\r\n")
            || transcript.ends_with('\n')
            || transcript.ends_with("\n\u{1b}[?2004l"),
        "the app restores the terminal before exiting: {transcript:?}"
    );
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t01_tui_opens_with_status_and_composer() {
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    let transcript = wait_for_normalized(&session, "> Nhập yêu cầu", Duration::from_secs(30));
    assert!(
        !transcript.contains("using the plain renderer"),
        "{transcript}"
    );
    let screen = normalized(&transcript);
    assert!(screen.contains("setup required"), "status row: {screen}");
    assert!(screen.contains("> Nhập yêu cầu"), "composer: {screen}");
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t03_pty_paste_keeps_newlines() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    let accepted_before = session.transcript().matches("[run] accepted").count();

    session.send("\u{1b}[200~first line\r\nsecond line\u{1b}[201~");
    std::thread::sleep(Duration::from_millis(500));
    if session.transcript().matches("[run] accepted").count() != accepted_before {
        // Measured limitation of this ConPTY: it strips the bracket markers and
        // turns the embedded newline into Enter before crossterm can emit Paste.
        // Keep this as an explicit capability result; the event-level T03 test is
        // the oracle on terminals that actually deliver a Paste event.
        eprintln!("T03_PTY_PASTE_UNAVAILABLE: ConPTY stripped bracketed-paste markers");
        assert!(
            session.is_alive(),
            "the unsupported paste must not crash the app"
        );
        session.send("\u{3}/exit\r");
        assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
        return;
    }
    session.send("\r");
    session.wait_for("fixture answer for: first line", Duration::from_secs(30));
    session.wait_for("second line", Duration::from_secs(30));
    assert_eq!(
        session.transcript().matches("[run] accepted").count(),
        accepted_before + 1,
        "the multiline paste is admitted as one message"
    );
    session.wait_for("[run] done", Duration::from_secs(30));
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t06_pty_approval_y_key() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("request approval fixture\r");
    session.wait_for("[approval] fixture_action", Duration::from_secs(30));
    // ConPTY can buffer a lone printable byte even while the child is in raw
    // mode. Submit the answer with CR so the test observes the same input path
    // as a user pressing `y` followed by Enter on affected Windows hosts.
    session.send("y\r");
    wait_for_normalized(
        &session,
        "[approval] granted fixture-approval-1",
        Duration::from_secs(30),
    );
    session.wait_for("[tool] fixture_action", Duration::from_secs(30));
    session.wait_for("[run] done", Duration::from_secs(30));
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g06_pty_at_picker() {
    let (temp, project) = sandbox();
    std::fs::write(project.join("notes.txt"), "picker fixture").expect("fixture file");
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("@");
    wait_for_normalized(&session, "TỆP", Duration::from_secs(30));
    session.send("\r");
    wait_for_normalized(&session, "notes.txt", Duration::from_secs(30));
    session.send("\u{3}/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g06_pty_bang_prefix_needs_approval() {
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("! echo SHOULD_NOT_RUN\r");
    let pending = wait_for_normalized(&session, "[approval]", Duration::from_secs(30));
    assert!(pending.contains("SHOULD_NOT_RUN"), "{pending}");
    session.send("n\r");
    wait_for_normalized(&session, "[approval] denied", Duration::from_secs(30));
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t_key_pty_key_with_spaces() {
    let (temp, project) = sandbox();
    let credential_dir = temp.path().join("credential fixture");
    let mut env = base_env(&temp);
    env.push(("HA_CREDENTIALS_DIR", credential_dir.display().to_string()));
    let mut session = PtySession::spawn(&project, &env);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("/key sk-with embedded spaces\r");
    wait_for_normalized(&session, "API key saved", Duration::from_secs(30));
    assert_eq!(
        std::fs::read_to_string(credential_dir.join("credentials.env")).expect("saved key"),
        "DEEPSEEK_API_KEY=\"sk-with embedded spaces\"\n"
    );
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t_more_pty_scroll_keys() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("show the transcript\r");
    session.wait_for("[run] done", Duration::from_secs(30));
    session.send("/more\r");
    session.wait_for("Esc đóng", Duration::from_secs(30));
    session.send("\u{1b}[6~\u{1b}[H\u{1b}[F");
    assert!(session.is_alive(), "scroll keys keep the panel open");
    session.send("\u{1b}/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn h05_pty_approval_denial_is_fail_closed() {
    let (temp, project) = sandbox();
    let source = "fn parse() { todo!() }\n";
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    let path = project.join("src/parser.rs");
    std::fs::write(&path, source).expect("source file");
    let expected_hash = harness_types::ContentHash::from_bytes(source.as_bytes())
        .as_str()
        .to_owned();
    let replacement = "fn parse() { println!(\"should not run\"); }\n";
    let (endpoint, second_request, server) =
        patch_then_answer_endpoint(expected_hash, replacement.to_owned());
    warm_up_loopback(&endpoint);

    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Harness Agents", Duration::from_secs(30));
    session.send("fix the parser\r");
    session.wait_for("[approval] ApplyPatch", Duration::from_secs(40));

    // Escape leaves approval pending. It neither grants nor denies the action.
    session.send("\u{1b}");
    std::thread::sleep(Duration::from_millis(250));
    assert!(session.is_alive(), "the TUI remains at approval");
    assert!(
        !second_request.load(Ordering::SeqCst),
        "Escape must not answer approval or advance the model"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("source remains readable"),
        source,
        "Escape must not execute the write"
    );

    session.send("n\r");
    let denied = wait_for_normalized(&session, "[approval] denied", Duration::from_secs(30));
    assert!(
        denied.contains("[tool] apply_patch") || denied.contains("[run] done"),
        "the real host reports the denied tool result:\n{denied}"
    );
    session.wait_for("[run] done", Duration::from_secs(30));
    assert!(
        second_request.load(Ordering::SeqCst),
        "the denial result reaches the provider continuation"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("source remains readable"),
        source,
        "a denied mutating action leaves the workspace unchanged"
    );
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
    server.join().expect("the denial fixture completes");
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g05_pty_always_allow_writes_local_rule() {
    let (temp, project) = sandbox();
    let source = "fn parse() { todo!() }\n";
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    std::fs::write(project.join("src/parser.rs"), source).expect("source file");
    let hash = harness_types::ContentHash::from_bytes(source.as_bytes())
        .as_str()
        .to_owned();
    let (endpoint, _, server) = patch_then_answer_endpoint(hash, "fn parse() {}\n".to_owned());
    warm_up_loopback(&endpoint);
    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("fix the parser\r");
    session.wait_for("[approval] ApplyPatch", Duration::from_secs(40));
    let rule = project.join(".harness/config.local.toml");
    assert!(!rule.exists(), "approval alone must not write a rule");
    session.send("A");
    wait_for_normalized(&session, "apply_patch", Duration::from_secs(30));
    assert!(!rule.exists(), "proposal alone must not write a rule");
    session.send("\r");
    session.wait_for("[run] done", Duration::from_secs(40));
    let local = std::fs::read_to_string(&rule).expect("confirmed rule");
    assert!(local.contains("apply_patch(src/parser.rs)"), "{local}");
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
    server.join().expect("fixture completes");
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g06_pty_ask_user_panel() {
    let (temp, project) = sandbox();
    let (endpoint, stop, server) = ask_user_endpoint();
    warm_up_loopback(&endpoint);
    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("choose a color\r");
    wait_for_normalized(
        &session,
        "Which color should I use?",
        Duration::from_secs(40),
    );
    session.send("\u{1b}/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
    stop.store(true, Ordering::SeqCst);
    server.join().expect("fixture completes");
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g08_pty_undo_panel() {
    let (temp, project) = sandbox();
    let source = "fn parse() { todo!() }\n";
    let replacement = "fn parse() { println!(\"fixed\"); }\n";
    std::fs::create_dir_all(project.join("src")).expect("source directory");
    let file = project.join("src/parser.rs");
    std::fs::write(&file, source).expect("source file");
    let hash = harness_types::ContentHash::from_bytes(source.as_bytes())
        .as_str()
        .to_owned();
    let (endpoint, _, server) = patch_then_answer_endpoint(hash, replacement.to_owned());
    warm_up_loopback(&endpoint);
    let mut session = PtySession::spawn(&project, &provider_env(&temp, &endpoint));
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("fix the parser\r");
    session.wait_for("[approval] ApplyPatch", Duration::from_secs(40));
    session.send("y\r");
    session.wait_for("[run] done", Duration::from_secs(40));
    assert_eq!(
        std::fs::read_to_string(&file).expect("patched file"),
        replacement
    );
    session.send("/undo\r");
    wait_for_normalized(&session, "[approval] WriteFile", Duration::from_secs(30));
    session.send("y\r");
    wait_for_occurrences(&session, "[run] done", 2, Duration::from_secs(40));
    assert_eq!(
        std::fs::read_to_string(&file).expect("restored file"),
        source
    );
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
    server.join().expect("fixture completes");
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t07_pty_plain_flag() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--plain"], &[]);
    let transcript = session.wait_for("using the plain renderer", Duration::from_secs(30));
    assert!(
        transcript.contains("plain renderer requested"),
        "{transcript}"
    );
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t07_pty_no_color() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &[], &[("NO_COLOR", "1")]);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
    let transcript = session.transcript();
    assert!(
        !has_color_sgr(&transcript),
        "NO_COLOR may retain cursor control but not palette SGR: {transcript:?}"
    );
}

#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn t07_pty_resize_keeps_the_draft() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));
    session.send("draft resize");
    // Raw PTY bytes interleave cursor moves inside a typed word. The submitted
    // history row below is the reliable oracle that resize preserved the draft.
    std::thread::sleep(Duration::from_millis(500));
    assert!(session.is_alive(), "typing keeps the TUI alive");
    session.resize(72, 20);
    session.send(" survives\r");
    session.wait_for("> draft resize survives", Duration::from_secs(30));
    session.wait_for(
        "fixture answer for: draft resize survives",
        Duration::from_secs(30),
    );
    session.send("/exit\r");
    assert_eq!(session.wait_exit(Duration::from_secs(20)), Some(0));
}

/// I14 (interactive half): the artifact a user installs, not the build-tree binary.
///
/// The installer self test proves the installed digest matches the built artifact;
/// this case proves the installed copy itself opens the app. It is staged under a
/// path with spaces and Vietnamese diacritics, started from a project directory
/// that is neither the install directory nor a Git repository, with PATH and the
/// profile variables rebuilt so no toolchain or developer state is reachable.
#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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
    let text = wait_for_normalized(&session, "> Nhập yêu cầu", Duration::from_secs(30));
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
        text.contains(&format!("Data: {} [HA_HOME]", data_root.display())),
        "the header reports the caller's data root with its origin: {text}"
    );
    assert!(
        text.contains(&format!("Store: {}", data_root.join("projects").display())),
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
#[test]
fn i06_pty_keeps_vietnamese_input_and_paste_intact() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Harness Agents", Duration::from_secs(30));

    session.send("sửa lỗi parser");
    // Raw PTY bytes are a repaint log rather than a screen, so submit the draft
    // and use the fixture echo as the oracle for what the editor actually held.
    // BS is the byte ConPTY reports for the Backspace virtual key; DEL is the
    // forward-delete key and correctly does nothing at the end of this draft.
    session.send("\u{8}!\r");
    session.wait_for(
        "fixture answer for: sửa lỗi parse!",
        Duration::from_secs(30),
    );
    session.wait_for("[run] done", Duration::from_secs(30));

    // A paste must never turn into several submitted commands. Whether the console
    // forwards the bracketed-paste markers is the console's choice: this ConPTY
    // build does not, and then the newline inside the paste arrives as Enter. Both
    // outcomes are asserted, so the test measures the app instead of the terminal.
    let accepted_before_paste = session.transcript().matches("[run] accepted").count();
    session.send("\u{1b}[200~multi\r\nline\u{1b}[201~");
    let pasted = session.wait_for_any(&["multi line", "multi"], Duration::from_secs(15));
    let transcript = session.transcript();
    let Some(_) = pasted else {
        panic!("the pasted text never reached the prompt:\n{transcript}");
    };
    if transcript.contains("multi line") {
        assert_eq!(
            transcript.matches("[run] accepted").count(),
            accepted_before_paste,
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
        session.send("\u{3}ok\r");
        session.wait_for("fixture answer for: ok", Duration::from_secs(30));
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
#[test]
fn i21_pty_survives_a_multiline_draft_and_keeps_the_prompt_usable() {
    let (temp, project) = sandbox();
    let mut session =
        PtySession::spawn_process(&project, &base_env(&temp), &["chat", "--fixture"], &[]);
    session.wait_for("Harness Agents", Duration::from_secs(30));

    // What this console does with a line break was measured, not assumed:
    // sending a bare line feed either inserts a break (a console that reports the
    // key distinctly, as most Unix terminals do) or arrives as Enter (measured on
    // this ConPTY build, exactly like the bracketed paste in i06). Both outcomes
    // are asserted, so the test measures the app rather than the terminal.
    session.send("dòng một");
    std::thread::sleep(Duration::from_millis(250));
    session.send("\n");
    session.send("dòng hai");
    std::thread::sleep(Duration::from_millis(250));

    let transcript = session.transcript();
    let inserted_break = !transcript.contains("[run] accepted");
    if inserted_break {
        assert!(
            !transcript.contains("> dòng hai"),
            "the continuation row must not carry the prompt marker:\n{transcript}"
        );
    } else {
        eprintln!(
            "i21: this console reports the line-feed key as Enter; asserting the app survives it"
        );
    }

    // Either way the app must still be alive and the prompt usable, which is what
    // a user needs after typing or pasting several rows.
    assert!(
        session.is_alive(),
        "the app stays alive through a multi-row draft:\n{transcript}"
    );
    if inserted_break {
        // Submitting the draft makes the in-memory editor state observable through
        // both the committed history row and the fixture service response.
        session.send("\r");
        session.wait_for("> dòng một", Duration::from_secs(30));
        session.wait_for("fixture answer for: dòng một", Duration::from_secs(30));
        session.wait_for("[run] done", Duration::from_secs(30));
    } else {
        session.send("\u{3}");
    }
    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
#[test]
fn i07a_ctrl_c_clears_an_idle_prompt() {
    // One pseudo-console per test: opening a second one in the same process blocks
    // on this host, so the idle and running phases cannot share one test.
    let (temp, project) = sandbox();
    let mut session = PtySession::spawn(&project, &base_env(&temp));
    session.wait_for("Harness Agents", Duration::from_secs(30));

    session.send("typo");
    wait_for_normalized(&session, "typo", Duration::from_secs(15));
    session.send("\u{3}");
    session.send("z");
    wait_for_normalized(&session, "z", Duration::from_secs(15));
    assert!(
        !session.transcript().contains("> typoz"),
        "an idle Ctrl-C clears the buffer:\n{}",
        session.transcript()
    );
    assert!(session.is_alive());

    // Clear the marker character first: "/exit" appended to it would be submitted
    // as a request instead of a command.
    session.send("\u{3}");
    wait_for_normalized(&session, ">", Duration::from_secs(10));
    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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
        let mut socket = accept_complete_request(&listener);
        asked_flag.store(true, Ordering::SeqCst);
        write_sse(&mut socket, &tool_call);
        // 2. After the tool settles the app asks again: hold this call open so the
        //    test can kill the process with the receipt already committed.
        let second = accept_complete_request(&listener);
        continued_flag.store(true, Ordering::SeqCst);
        let _ = held.recv_timeout(Duration::from_mins(2));
        drop(second);
        // 3. The continuation after the kill gets a prose answer. A warm-up connect
        //    that sends no request is not that call.
        let mut third = accept_complete_request(&listener);
        write_sse(
            &mut third,
            "data: {\"choices\":[{\"delta\":{\"content\":\"the parser is already fixed; nothing to redo\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
    });
    (
        format!("http://{address}/chat/completions"),
        asked,
        continued,
        release,
        handle,
    )
}

/// Real provider fixture for the H05 fail-closed approval case: the first answer
/// requests a patch, and the second acknowledges the result after the user denies it.
fn patch_then_answer_endpoint(
    expected_hash: String,
    replacement: String,
) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().expect("fixture address");
    let second_request = Arc::new(AtomicBool::new(false));
    let second_flag = Arc::clone(&second_request);
    let server = std::thread::spawn(move || {
        let arguments = serde_json::json!({
            "path": "src/parser.rs",
            "expected_hash": expected_hash,
            "replacement": replacement,
        })
        .to_string();
        let tool_call = [
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "id": "deny-patch",
                            "function": {
                                "name": "apply_patch",
                                "arguments": serde_json::Value::String(arguments),
                            },
                        }]},
                        "finish_reason": null,
                    }],
                })
            ),
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                })
            ),
            "data: [DONE]\n\n".to_owned(),
        ]
        .concat();
        let mut first = accept_complete_request(&listener);
        write_sse(&mut first, &tool_call);
        let mut second = accept_complete_request(&listener);
        second_flag.store(true, Ordering::SeqCst);
        write_sse(
            &mut second,
            "data: {\"choices\":[{\"delta\":{\"content\":\"The patch was denied; the file is unchanged.\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
    });
    (
        format!("http://{address}/chat/completions"),
        second_request,
        server,
    )
}

fn ask_user_endpoint() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    listener.set_nonblocking(true).expect("nonblocking fixture");
    let address = listener.local_addr().expect("fixture address");
    let stop = Arc::new(AtomicBool::new(false));
    let server_stop = Arc::clone(&stop);
    let server = std::thread::spawn(move || {
        let arguments = serde_json::json!({
            "question": "Which color should I use?",
            "options": ["blue", "green"]
        })
        .to_string();
        let first = [
            format!(
                "data: {}\n\n",
                serde_json::json!({"choices":[{"delta":{"tool_calls":[{
                    "id":"ask-color",
                    "function":{"name":"ask_user","arguments":arguments}
                }]},"finish_reason":null}]})
            ),
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n".to_owned(),
            "data: [DONE]\n\n".to_owned(),
        ]
        .concat();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first}",
            first.len()
        );
        let deadline = Instant::now() + Duration::from_mins(1);
        while !server_stop.load(Ordering::SeqCst) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    let _ = socket.set_nonblocking(false);
                    if read_http_request(&mut socket).is_ok_and(|request| !request.is_empty()) {
                        let _ = socket.write_all(response.as_bytes());
                        let _ = socket.flush();
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (format!("http://{address}/chat/completions"), stop, server)
}

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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
    warm_up_loopback(&endpoint);

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

/// Accept a complete provider request before replying. CP-B advertises larger
/// tool schemas, so one socket read may contain only part of the request body.
fn accept_complete_request(listener: &std::net::TcpListener) -> std::net::TcpStream {
    loop {
        let (mut socket, _) = listener.accept().expect("the model call arrives");
        if read_http_request(&mut socket).is_ok_and(|request| !request.is_empty()) {
            return socket;
        }
    }
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

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
#[test]
#[allow(clippy::too_many_lines)] // One kill-then-reopen sequence; splitting it hides the order.
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

    let (answer_endpoint, answer_contacted, answer_server) = sse_answer("the store is free again");
    warm_up_loopback(&answer_endpoint);
    let mut follow_up = std::process::Command::new(cli_binary())
        .args(["chat", "--headless", "--prompt", "after the exit", "--json"])
        .current_dir(&project)
        .env("HA_HOME", ha_home(&temp))
        .env("HA_PROVIDER_ENDPOINT", &answer_endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .env("HA_TEST_TRACE_HEADLESS", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("the next host starts");
    let follow_up_deadline = Instant::now() + Duration::from_secs(30);
    let follow_up_status = loop {
        if let Some(status) = follow_up.try_wait().expect("next host status") {
            break status;
        }
        if Instant::now() >= follow_up_deadline {
            let _ = follow_up.kill();
            let _ = follow_up.wait();
            let mut stderr = String::new();
            follow_up
                .stderr
                .take()
                .expect("timed out host stderr")
                .read_to_string(&mut stderr)
                .expect("timed out host stderr read");
            panic!(
                "the next host exceeded its bound (provider_contacted={}): {stderr}",
                answer_contacted.load(Ordering::SeqCst),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut follow_up_stdout = Vec::new();
    follow_up
        .stdout
        .take()
        .expect("next host stdout")
        .read_to_end(&mut follow_up_stdout)
        .expect("next host stdout read");
    let mut follow_up_stderr = Vec::new();
    follow_up
        .stderr
        .take()
        .expect("next host stderr")
        .read_to_end(&mut follow_up_stderr)
        .expect("next host stderr read");
    answer_server.join().expect("the answer fixture finishes");
    assert!(
        follow_up_status.success(),
        "the next host took the store: {}",
        String::from_utf8_lossy(&follow_up_stderr)
    );
    let parsed: serde_json::Value =
        serde_json::from_slice(&follow_up_stdout).expect("the next host prints JSON");
    assert_eq!(
        parsed["response"],
        serde_json::json!("the store is free again"),
        "the next host completed its own turn: {parsed}"
    );
}

/// One-shot SSE provider that answers one request with plain text.
fn sse_answer(text: &str) -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("answer listener");
    listener
        .set_nonblocking(true)
        .expect("the answer listener is non-blocking");
    let address = listener.local_addr().expect("answer address");
    let text = text.to_owned();
    let contacted = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&contacted);
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(35);
        let mut socket = loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket
                        .set_nonblocking(false)
                        .expect("an accepted answer socket can block for its request");
                    socket
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("the answer socket read is bounded");
                    if read_http_request(&mut socket).is_ok_and(|request| !request.is_empty()) {
                        break socket;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "the answer provider was not contacted before its deadline"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("the answer provider failed: {error}"),
            }
        };
        flag.store(true, Ordering::SeqCst);
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
        );
        write_sse(&mut socket, &body);
    });
    (
        format!("http://{address}/chat/completions"),
        contacted,
        handle,
    )
}

// ---------------------------------------------------------------------------
// I12 - a configured but unreachable provider (the offline path)
// ---------------------------------------------------------------------------

#[ignore = "needs a real console: ConPTY only delivers a transcript when the process that creates the pseudo-console owns one, and a sandboxed cargo test does not. Run scripts/Invoke-HaPtyAcceptance.ps1 (bounded, new console, transcript per filter) - all ten cases i01, i05, i06, i07a, i07b, i08, i12, i13, i14 and i21 pass there."]
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

/// A turn that stops at a bound continues by itself, in a real terminal.
///
/// The measured complaint: eight steps, tokens to spare, and the turn stopped —
/// `[run] paused: step limit reached · 8 steps · 14 tool calls · 46.8s` — as if the task
/// were broken. A bound stops a loop that has gone wrong, not a task that is still
/// moving, so the app sends the next request itself, says so, and stops doing that once
/// the budget the environment set is spent.
#[ignore = "needs a real console; run scripts/Invoke-HaPtyAcceptance.ps1"]
#[test]
fn g4_a_step_bound_continues_the_turn_by_itself() {
    let (temp, project) = sandbox();
    let provider = ToolCallProvider::start();
    let mut env = base_env(&temp);
    env.push(("HA_PROVIDER_ENDPOINT", provider.endpoint()));
    env.push(("HA_PROVIDER_MODEL", "fixture-model".to_owned()));
    env.push(("DEEPSEEK_API_KEY", "fixture-secret-value".to_owned()));
    // Two model calls per turn and one continuation: the first turn runs the tool the
    // model asked for and then has no step left for the round after it, and only that
    // first pause may be continued.
    env.push(("HA_TURN_MAX_STEPS", "2".to_owned()));
    env.push(("HA_TURN_CONTINUATIONS", "1".to_owned()));
    let mut session = PtySession::spawn(&project, &env);
    session.wait_for("Nhập yêu cầu", Duration::from_secs(30));

    session.send("list the files\r");
    // The read is gated, so the first turn needs the answer before the tool can run and
    // the bound can be reached. Every wait counts occurrences: the same panel and the
    // same card come back for the continuation, and matching the first one again would
    // answer a panel that is no longer there.
    wait_for_occurrences(&session, "[approval] ListFiles", 1, Duration::from_secs(40));
    session.send("y\r");
    wait_for_occurrences(
        &session,
        "list_files {\"path\": \".\"} ok",
        1,
        Duration::from_secs(40),
    );

    let continued = wait_for_normalized(
        &session,
        "[auto] continue: the previous turn stopped at a bound",
        Duration::from_secs(40),
    );
    assert!(
        continued.contains("[info] step limit reached; continuing automatically (1 of 1)"),
        "the app says why it spoke:\n{continued}"
    );
    assert!(
        continued.contains("[run] paused: step limit reached · 2 steps · 1 tool calls"),
        "the pause is still on the record before the continuation:\n{continued}"
    );

    // The continuation runs, and this time the budget is spent: the pause stands.
    wait_for_occurrences(&session, "[approval] ListFiles", 2, Duration::from_secs(40));
    session.send("y\r");
    wait_for_occurrences(
        &session,
        "list_files {\"path\": \".\"} ok",
        2,
        Duration::from_secs(40),
    );
    let paused = wait_for_occurrences(
        &session,
        "[run] paused: step limit reached",
        2,
        Duration::from_secs(40),
    );
    assert_eq!(
        paused.matches("[auto] continue").count(),
        1,
        "the budget was one, so exactly one request was the app's own:\n{paused}"
    );
    assert!(
        provider.requests() >= 4,
        "two model calls in each of the two turns reached the provider: {} request(s)",
        provider.requests()
    );

    session.send("/exit\r");
    assert_eq!(
        session.wait_exit(Duration::from_secs(20)),
        Some(0),
        "transcript:\n{}",
        session.transcript()
    );
}
