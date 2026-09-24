//! G13 automation contract through the compiled CLI and a disposable project.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_ha")
}

fn command(root: &std::path::Path) -> Command {
    let mut command = Command::new(binary());
    command
        .current_dir(root)
        .env("HA_HOME", root)
        .stdin(Stdio::null());
    command
}

fn run(root: &std::path::Path, args: &[&str]) -> Output {
    command(root).args(args).output().expect("ha exec runs")
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

#[test]
fn g13_stream_json_emits_one_event_per_line_in_order() {
    let root = tempfile::tempdir().expect("temporary project");
    let output = run(
        root.path(),
        &["exec", "hello", "--mock", "--output-format", "stream-json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert!(lines.len() >= 3, "{lines:?}");
    let events = lines
        .iter()
        .map(|line| {
            serde_json::from_slice::<serde_json::Value>(line).expect("one JSON event per line")
        })
        .collect::<Vec<_>>();
    assert_eq!(events.first().unwrap()["type"], "turn.started");
    assert!(events.iter().any(|event| event["type"] == "text.delta"));
    assert_eq!(events.last().unwrap()["type"], "run.terminal");
    assert_eq!(events.last().unwrap()["data"]["stop"], "final");
}

#[test]
fn g13_stdin_prompt_requires_the_explicit_dash() {
    let root = tempfile::tempdir().expect("temporary project");
    let missing = run(root.path(), &["exec", "--mock"]);
    assert_eq!(missing.status.code(), Some(2));
    let mut child = command(root.path())
        .args(["exec", "--prompt", "-", "--mock", "--output-format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ha exec");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(b"prompt from stdin")
        .expect("write prompt");
    let output = child.wait_with_output().expect("ha exec completes");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json(&output)["schema_version"], 1);
    assert_eq!(json(&output)["acceptance"]["state"], "not_evaluated");
}

#[test]
fn g13_headless_never_emits_ansi() {
    let root = tempfile::tempdir().expect("temporary project");
    for format in ["text", "json", "stream-json"] {
        let output = run(
            root.path(),
            &["exec", "hello", "--mock", "--output-format", format],
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "{format}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.contains(&27), "{format} stdout had ANSI");
        assert!(!output.stderr.contains(&27), "{format} stderr had ANSI");
    }
}

#[test]
fn g13_continue_uses_the_newest_session_in_this_project() {
    let root = tempfile::tempdir().expect("temporary project");
    let first = run(
        root.path(),
        &["exec", "first", "--mock", "--output-format", "json"],
    );
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first = json(&first);
    let second = run(
        root.path(),
        &[
            "exec",
            "second",
            "--mock",
            "--continue",
            "--output-format",
            "json",
        ],
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = json(&second);
    assert_eq!(second["resumed_from"], first["session_id"]);
    assert_eq!(second["task_id"], first["task_id"]);
}

fn ask_user_fixture() -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<usize>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback fixture");
    listener
        .set_nonblocking(true)
        .expect("bounded fixture accept");
    let endpoint = format!(
        "http://{}/chat/completions",
        listener.local_addr().expect("fixture address")
    );
    let (stop, stop_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut served = 0;
        loop {
            if stop_rx.try_recv().is_ok() || std::time::Instant::now() >= deadline {
                break;
            }
            let (mut socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            };
            socket
                .set_nonblocking(false)
                .expect("blocking fixture socket");
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("bounded request read");
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            let head_end = loop {
                let size = std::io::Read::read(&mut socket, &mut chunk).unwrap_or_default();
                if size == 0 {
                    break None;
                }
                request.extend_from_slice(&chunk[..size]);
                if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break Some(end + 4);
                }
                assert!(request.len() < 4 * 1024 * 1024, "request head too large");
            };
            let Some(head_end) = head_end else {
                continue; // readiness probe, not a model request
            };
            let head = String::from_utf8_lossy(&request[..head_end]).to_lowercase();
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < head_end + length {
                let size = std::io::Read::read(&mut socket, &mut chunk).expect("read request body");
                assert!(size > 0, "request body ended early");
                request.extend_from_slice(&chunk[..size]);
            }
            let call = serde_json::json!({
                "choices": [{
                    "delta": {"tool_calls": [{
                        "index": 0,
                        "id": "ask-1",
                        "type": "function",
                        "function": {"name": "ask_user", "arguments": "{\"question\":\"Which color?\",\"options\":[\"blue\",\"green\"]}"}
                    }]},
                    "finish_reason": null
                }]
            });
            let finish =
                serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]});
            let body = format!("data: {call}\n\ndata: {finish}\n\ndata: [DONE]\n\n");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .expect("send ask_user stream");
            socket.flush().expect("flush response");
            socket
                .shutdown(std::net::Shutdown::Write)
                .expect("half-close response");
            let mut trailing = [0_u8; 256];
            loop {
                match std::io::Read::read(&mut socket, &mut trailing) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            served += 1;
        }
        served
    });
    (endpoint, stop, server)
}

#[test]
fn g13_exit_code_3_when_the_model_asks() {
    let root = tempfile::tempdir().expect("temporary project");
    let (endpoint, stop, server) = ask_user_fixture();
    let output = command(root.path())
        .args(["exec", "ask", "--output-format", "stream-json"])
        .env("HA_PROVIDER_ENDPOINT", &endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .output()
        .expect("ha exec runs");
    let _ = stop.send(());
    assert!(
        server.join().expect("fixture completes") >= 1,
        "fixture received no model request"
    );
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).expect("NDJSON event"))
        .collect::<Vec<_>>();
    assert!(lines.iter().any(|event| event["type"] == "ask_user"));
    assert_eq!(
        lines.last().expect("terminal event")["type"],
        "run.terminal"
    );
    assert_eq!(lines.last().unwrap()["data"]["stop"], "needs_input");
}

#[tokio::test]
async fn g13_goal_satisfied_emits_acceptance_command() {
    let root = tempfile::tempdir().expect("temporary project");
    let output = run(
        root.path(),
        &[
            "exec",
            "hello",
            "--mock",
            "--goal",
            "answer",
            "--output-format",
            "json",
        ],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = json(&output);
    assert_eq!(result["acceptance"]["state"], "satisfied");
    let command_id = result["acceptance"]["command_id"]
        .as_str()
        .expect("durable command id");
    let task_id =
        harness_types::TaskId::parse(result["task_id"].as_str().expect("task id").to_owned())
            .expect("canonical task id");
    let projects = root.path().join("data").join("projects");
    let store_dir = std::fs::read_dir(projects)
        .expect("project stores")
        .next()
        .expect("one project store")
        .expect("store entry")
        .path();
    let store = harness_store_sqlite::SqliteStore::open_read_only(store_dir)
        .await
        .expect("read project store");
    let (stored_id, record) = store
        .task_acceptance(&task_id)
        .await
        .expect("read acceptance")
        .expect("accepted task");
    assert_eq!(stored_id.as_str(), command_id);
    assert!(record.is_accepted());
    store.close().await.expect("close project store");
}
