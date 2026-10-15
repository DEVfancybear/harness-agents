//! Knowledge administration and native headless access through the compiled binary.
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn run(home: &Path, project: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(["knowledge", "--json", "--cwd"])
        .arg(project)
        .args(args)
        .env("HA_HOME", home)
        .env("HA_OFFLINE", "1")
        .output()
        .expect("run ha knowledge")
}

fn json(output: Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8(output.stdout).expect("UTF-8 output"))
        .expect("versioned JSON")
}

fn payload(output: Output) -> serde_json::Value {
    let json = json(output);
    serde_json::from_str(json["lines"][0].as_str().unwrap()).unwrap()
}

#[test]
fn knowledge_persists_controls_and_recalls_external_edits_across_processes() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let project = fixture.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let status = json(run(&home, &project, &[]));
    assert_eq!(status["schema_version"], 1);
    assert!(
        !project.join(".harness/knowledge").exists(),
        "read remains lazy"
    );
    json(run(&home, &project, &["off"]));
    assert!(!run(&home, &project, &["search", "fact"]).status.success());
    json(run(&home, &project, &["on"]));
    let pages = project.join(".harness/knowledge");
    std::fs::write(
        pages.join("page with spaces.md"),
        "# Topic\nDurable fact: ZEPHYR-481\n",
    )
    .unwrap();
    let found = payload(run(&home, &project, &["search", "ZEPHYR-481"]));
    assert_eq!(found["results"][0]["path"], "page with spaces.md");
    let read = payload(run(&home, &project, &["read", "page with spaces.md"]));
    assert!(
        read["results"][0]["content"]
            .as_str()
            .unwrap()
            .contains("ZEPHYR-481")
    );
    let first_hash = read["results"][0]["hash"].clone();
    std::fs::write(
        pages.join("page with spaces.md"),
        "# Topic\nUpdated fact: MERIDIAN-792\n",
    )
    .unwrap();
    let read = payload(run(&home, &project, &["read", "page with spaces.md"]));
    assert_ne!(read["results"][0]["hash"], first_hash);
    assert!(
        payload(run(&home, &project, &["search", "ZEPHYR-481"]))["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    json(run(&home, &project, &["refresh"]));
    assert!(
        std::fs::read_to_string(pages.join("index.md"))
            .unwrap()
            .contains("page%20with%20spaces.md")
    );
    std::fs::remove_file(pages.join("page with spaces.md")).unwrap();
    assert!(
        payload(run(&home, &project, &["list"]))["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !run(&home, &project, &["read", "../outside.md"])
            .status
            .success()
    );
    assert!(!run(&home, &project, &["on", "unexpected"]).status.success());
}

#[test]
fn knowledge_global_scope_and_project_disable_do_not_leak_between_projects() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    json(run(&home, &first, &["on", "--global"]));
    // HA_HOME resolves data underneath home/data.
    let global = home.join("data/knowledge");
    std::fs::write(global.join("global.md"), "# Shared\nGLOBAL-PROBE-219\n").unwrap();
    json(run(&home, &first, &["on"]));
    std::fs::write(
        first.join(".harness/knowledge/local.md"),
        "# Local\nLOCAL-PROBE-387\n",
    )
    .unwrap();
    assert_eq!(
        payload(run(&home, &first, &["list"]))["results"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        payload(run(&home, &second, &["list"]))["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!run(&home, &second, &["read", "local.md"]).status.success());
    json(run(&home, &first, &["off"]));
    assert!(
        !run(&home, &first, &["read", "global.md", "--global"])
            .status
            .success()
    );
    assert!(
        run(&home, &second, &["read", "global.md", "--global"])
            .status
            .success()
    );
    json(run(&home, &second, &["off", "--global"]));
    assert!(
        !run(&home, &second, &["read", "global.md", "--global"])
            .status
            .success()
    );
}

#[cfg(windows)]
#[test]
fn knowledge_refuses_a_junction_in_the_project_container() {
    let fixture = tempfile::tempdir().unwrap();
    let project = fixture.path().join("project");
    let outside = fixture.path().join("outside");
    std::fs::create_dir(&project).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let script = fixture.path().join("junction.ps1");
    std::fs::write(&script, "param([string]$LinkPath,[string]$TargetPath)\n$ErrorActionPreference='Stop'\nNew-Item -ItemType Junction -Path $LinkPath -Target $TargetPath | Out-Null\n").unwrap();
    let created = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(script)
        .arg("-LinkPath")
        .arg(project.join(".harness"))
        .arg("-TargetPath")
        .arg(&outside)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let refused = run(&fixture.path().join("home"), &project, &["on"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("reparse point"));
    assert!(!outside.join("knowledge").exists());
}

fn read_model_request(socket: &mut std::net::TcpStream) -> Option<serde_json::Value> {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut request = Vec::new();
    let mut chunk = [0_u8; 8192];
    let head_end = loop {
        let count = socket.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..count]);
        assert!(request.len() < 4 * 1024 * 1024, "fixture request too large");
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&request[..head_end]).to_ascii_lowercase();
    let length = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|length| length.trim().parse::<usize>().ok())
        .expect("provider declares its request body length");
    assert!(length < 4 * 1024 * 1024, "fixture body too large");
    while request.len() < head_end + length {
        let count = socket.read(&mut chunk).ok()?;
        if count == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..count]);
    }
    Some(serde_json::from_slice(&request[head_end..head_end + length]).unwrap())
}

fn knowledge_model_reply(request: &serde_json::Value, enabled: bool) -> String {
    let calls = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .count();
    let arguments = match calls {
        0 if enabled => Some(serde_json::json!({"action":"search","query":"HEADLESS-PROBE-741"})),
        1 if enabled => Some(serde_json::json!({"action":"read","path":"probe.md"})),
        _ => None,
    };
    let (delta, finish) = if let Some(arguments) = arguments {
        (
            serde_json::json!({"tool_calls":[{
                "index":0,"id":format!("knowledge-{calls}"),"type":"function",
                "function":{"name":"knowledge","arguments":arguments.to_string()}
            }]}),
            "tool_calls",
        )
    } else {
        (serde_json::json!({"content":"fixture completed"}), "stop")
    };
    let event = serde_json::json!({"choices":[{"delta":delta,"finish_reason":null}]});
    let done = serde_json::json!({"choices":[{"delta":{},"finish_reason":finish}]});
    format!("data: {event}\n\ndata: {done}\n\ndata: [DONE]\n\n")
}

fn headless_requests(home: &Path, project: &Path, enabled: bool) -> Vec<serde_json::Value> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
    let (stop, stopped) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(45);
        while stopped.try_recv().is_err() && Instant::now() < deadline {
            let (mut socket, _) = match listener.accept() {
                Ok(pair) => pair,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            };
            let Some(request) = read_model_request(&mut socket) else {
                continue;
            };
            let body = knowledge_model_reply(&request, enabled);
            requests.push(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).unwrap();
            socket.flush().unwrap();
            socket.shutdown(std::net::Shutdown::Write).unwrap();
            while socket.read(&mut [0_u8; 256]).is_ok_and(|count| count != 0) {}
        }
        requests
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_ha"))
        .args([
            "chat",
            "--headless",
            "--prompt",
            "Inspect the available knowledge.",
            "--json",
        ])
        .current_dir(project)
        .env("HA_HOME", home)
        .env("HA_OFFLINE", "1")
        .env("HA_REPL", "off")
        .env("HA_WEB", "off")
        .env("HA_AUTO_REFINE", "off")
        .env("HA_PROVIDER_ENDPOINT", endpoint)
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait().unwrap().is_none() {
        child.kill().unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let _ = stop.send(());
    let requests = server.join().unwrap();
    let result = json(output);
    assert_eq!(result["response"], "fixture completed");
    assert_eq!(result["tool_calls"], if enabled { 2 } else { 0 });
    requests
}

#[test]
fn headless_exposes_and_executes_native_knowledge_and_hides_it_when_disabled() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let project = fixture.path().join("project");
    std::fs::create_dir(&project).unwrap();
    json(run(&home, &project, &["on"]));
    std::fs::write(
        project.join(".harness/knowledge/probe.md"),
        "# Headless probe\nSource fact HEADLESS-PROBE-741\n",
    )
    .unwrap();
    let requests = headless_requests(&home, &project, true);
    assert!(!requests.is_empty(), "the model received a request");
    assert!(requests.iter().all(|request| {
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "knowledge")
    }));
    // The HTTP adapter may retry a request. Verify the tool sequence rather
    // than its transport attempt count.
    let receipts = requests
        .iter()
        .map(|request| {
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|message| message["role"] == "tool")
                .map(|message| message["content"].as_str().unwrap())
                .collect::<Vec<_>>()
        })
        .find(|receipts| receipts.len() == 2)
        .expect("search and read receipts reached the next model request");
    assert!(receipts[0].contains("keyword_rrf") && receipts[0].contains("HEADLESS-PROBE-741"));
    assert!(receipts[1].contains("Source fact HEADLESS-PROBE-741"));

    json(run(&home, &project, &["off"]));
    let requests = headless_requests(&home, &project, false);
    assert!(!requests.is_empty(), "the disabled turn reached the model");
    assert!(requests.iter().all(|request| {
        !request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "knowledge")
    }));
    assert!(requests.iter().all(|request| {
        !request["messages"]
            .to_string()
            .contains("HEADLESS-PROBE-741")
    }));
}
