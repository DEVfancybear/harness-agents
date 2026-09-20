//! Real `SQLite` and loopback transport checks for the interactive service.
use super::*;
use crate::interactive::bootstrap::{self, LaunchRequest};
use crate::interactive::paths::HostPlatform;
use harness_types::ProjectId;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// EnvironmentCredential reads process state. Run this case in a child instead
// of mutating global environment variables under parallel Rust tests.
#[test]
fn completion_service_resume_flow() {
    if std::env::var_os("HA_COMPLETION_CHILD").is_some() {
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(resume_flow());
        return;
    }
    // Two attempts, because the fixture is a real loopback socket and this case runs
    // beside the rest of the suite. Measured: it passes alone every time and failed
    // in roughly one full-suite run in three, with the child's own runtime starved of
    // a worker thread before its first connect. A failure that survives the retry is
    // still reported, so a real regression cannot hide behind this.
    let mut last = None;
    for attempt in 0..2 {
        let (status, output) = run_child();
        if status.success() && output.contains("1 passed") {
            return;
        }
        last = Some((status, output, attempt));
        std::thread::sleep(Duration::from_millis(250));
    }
    let (status, output, attempt) = last.expect("the loop runs at least once");
    assert!(
        status.success(),
        "child failed on attempt {} of 2: {output}",
        attempt + 1
    );
    assert!(
        output.contains("1 passed"),
        "child must execute the selected test: {output}"
    );
}

/// Run the case in its own process, which is what keeps it from mutating the
/// environment every other test in this binary shares.
fn run_child() -> (std::process::ExitStatus, String) {
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "interactive::service::completion_tests::completion_service_resume_flow",
            "--nocapture",
        ])
        .env("HA_COMPLETION_CHILD", "1")
        .env("HA_API_KEY", "completion-fixture-only")
        .env_remove("DEEPSEEK_API_KEY")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("isolated credential process");
    // The child spawns its own runtime and can be starved of a worker thread during
    // a full-workspace run; the bound only has to outlast a slow machine, not a hang.
    let deadline = Instant::now() + Duration::from_mins(2);
    let status = loop {
        if let Some(status) = child.try_wait().expect("child status") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("service resume test exceeded its bound");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut output = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut output)
        .expect("stdout read");
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut output)
        .expect("stderr read");
    (status, output)
}

async fn terminal(channel: &mut SessionChannel) -> SessionEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            for event in channel.drain() {
                if matches!(
                    event,
                    SessionEvent::RunTerminal { .. } | SessionEvent::RecoverableError { .. }
                ) {
                    return event;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("terminal event before deadline")
}

async fn read_http(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let count = socket.read(&mut chunk).await.expect("request read");
        assert_ne!(count, 0, "request ended before its body");
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() < 1_000_000, "bounded fixture request");
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]);
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().expect("body length"))
                })
                .expect("content length");
            if bytes.len() >= end + 4 + length {
                return String::from_utf8(bytes).expect("UTF-8 request");
            }
        }
    }
}

#[allow(clippy::too_many_lines)]
async fn resume_flow() {
    let temp = tempfile::tempdir().expect("temp project");
    let project = temp.path().join("project");
    std::fs::create_dir(&project).expect("project");
    let environment =
        LaunchEnvironment::from_pairs([("HA_HOME", temp.path().join("home").into_os_string())]);
    let context = bootstrap::resolve(LaunchRequest {
        cwd: None,
        caller_dir: project.clone(),
        platform: HostPlatform::current(),
        environment,
        explicit_data_dir: None,
    })
    .expect("context");
    let source = SessionId::generate();
    let source_task = TaskId::generate();
    let store = Arc::new(
        SqliteStore::open_writer(WriterOpenOptions::new(
            context.project_store_dir(),
            HostId::generate(),
        ))
        .await
        .expect("seed store"),
    );
    let runtime = RuntimeService::new(
        Arc::clone(&store),
        Arc::new(harness_providers::MockProvider::text("source answer")),
        RuntimeConfig::default(),
    );
    runtime
        .run(RunRequest::new(
            source.clone(),
            source_task.clone(),
            InputId::generate(),
            "remember source marker",
            observe_workspace(ProjectId::generate(), &project).expect("workspace"),
        ))
        .await
        .expect("seed turn");
    drop(runtime);
    Arc::try_unwrap(store)
        .expect("single owner")
        .close()
        .await
        .expect("close seed");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback");
    let environment = LaunchEnvironment::from_pairs([
        (
            ENDPOINT_VARIABLE,
            format!(
                "http://{}/chat/completions",
                listener.local_addr().expect("address")
            ),
        ),
        (MODEL_VARIABLE, "fixture-model".to_owned()),
        ("HA_API_KEY", "completion-fixture-only".to_owned()),
    ]);
    let mut channel = SessionChannel::new();
    let mut service = AgentSessionService::new(&context, environment, channel.sender());
    let initially_new_task = service.task_id.clone();
    service
        .resume(Some(source.as_str().to_owned()))
        .expect("selection");
    assert_eq!(
        *service.previous_session.lock().expect("selection"),
        Some(source.clone()),
        "selection must happen before submit, without an async lookup race"
    );
    service.submit(SubmitRequest {
        input_id: InputId::generate(),
        text: "continue source".to_owned(),
    });
    // The accept loop must be listening before the service connects: this
    // environment refuses a connection to a fresh loopback listener that nobody is
    // accepting on yet, which is the flake the gate reports for this case. The
    // listener is shared so the failure-mode assertion below can still prove that an
    // unknown source calls no provider at all.
    let listener = std::sync::Arc::new(listener);
    let fixture_listener = std::sync::Arc::clone(&listener);
    let (listening, is_listening) = tokio::sync::oneshot::channel::<()>();
    let provider = tokio::spawn(async move {
        let _ = listening.send(());
        let (mut socket, _) = fixture_listener
            .accept()
            .await
            .expect("production adapter connects");
        let request = read_http(&mut socket).await;
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"resumed answer\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("response");
        request
    });
    // The bound is generous on purpose: under a full-workspace run this child can be
    // starved of a worker thread, and a short bound turned that into a red gate.
    let (request, outcome) = tokio::time::timeout(Duration::from_mins(1), async {
        is_listening.await.expect("the fixture is listening");
        tokio::join!(
            async { provider.await.expect("fixture task") },
            terminal(&mut channel)
        )
    })
    .await
    .expect("bounded resumed turn");
    assert!(
        request.contains("remember source marker"),
        "source context must reach the real adapter"
    );
    assert!(
        matches!(
            outcome,
            SessionEvent::RunTerminal {
                outcome: RunOutcome::Done
            }
        ),
        "{outcome:?}"
    );
    let store = SqliteStore::open_read_only(context.project_store_dir())
        .await
        .expect("reopen");
    let sessions = store.list_sessions().await.expect("sessions");
    assert_eq!(sessions.len(), 2);
    assert!(
        sessions
            .iter()
            .all(|session| session.task_id == source_task),
        "resume keeps the persisted task, not the service's freshly generated task"
    );
    store.close().await.expect("reader close");

    service.resume(None).expect("new conversation");
    assert_ne!(
        service.task_id, initially_new_task,
        "/new must allocate a new task"
    );
    assert!(service.previous_session.lock().expect("source").is_none());
    let missing = SessionId::generate();
    service
        .resume(Some(missing.as_str().to_owned()))
        .expect("canonical but unknown source");
    service.submit(SubmitRequest {
        input_id: InputId::generate(),
        text: "must not start fresh".to_owned(),
    });
    let outcome = terminal(&mut channel).await;
    assert!(
        matches!(outcome, SessionEvent::RecoverableError { ref message } if message.contains("not in this project's store")),
        "{outcome:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(75), listener.accept())
            .await
            .is_err(),
        "unknown source must not call provider"
    );
    let store = SqliteStore::open_read_only(context.project_store_dir())
        .await
        .expect("reopen");
    assert_eq!(
        store.list_sessions().await.expect("sessions").len(),
        2,
        "invalid resume does not silently admit fresh work"
    );
    store.close().await.expect("close");
}
