//! Time what the host adds to a turn, around the model.
//!
//! ```text
//! cargo build --release -p harness-cli
//! cargo run --release -p harness-cli --example turn_bench -- //!     [--steps N] [--runs N] [--model-ms N] [--ha PATH] [--each 1] [--keep DIR]
//! ```
//!
//! A loopback provider answers `ha exec` with `--steps` tool calls (reads,
//! searches, listings and writes in turn) and then an answer, each after
//! `--model-ms` of "thinking". The project is a small Git repository, and
//! every run shares one home and project as a user's runs do (the first run,
//! which creates the store, is reported apart).
//!
//! The time between the end of one reply and the arrival of the next request
//! is the host's own work for one step: the gate, the tool, the journal and the
//! next request. Start-up is spawn to first request, the end is last reply to
//! exit, and "whole run less the model" is everything the host added. `--ha`
//! compares another build; `--each` prints every step; `--keep` leaves the
//! home and project in `DIR` for inspection.

use std::{
    fmt::Write as _,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn arg(name: &str) -> Option<String> {
    let mut words = std::env::args().skip(1);
    while let Some(word) = words.next() {
        if word == name {
            return words.next();
        }
    }
    None
}

fn number(name: &str, default: usize) -> usize {
    arg(name).and_then(|n| n.parse().ok()).unwrap_or(default)
}

fn read_request(socket: &mut TcpStream) -> Option<serde_json::Value> {
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 8192];
    let header_end = loop {
        let read = socket.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
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
        let read = socket.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    serde_json::from_slice(&buffer[header_end..]).ok()
}

fn frame(value: &serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

fn reply(step: usize, steps: usize) -> String {
    if step >= steps {
        return format!(
            "{}{}data: [DONE]\n\n",
            frame(
                &serde_json::json!({"choices": [{"delta": {"content": "done"}, "finish_reason": null}]})
            ),
            frame(&serde_json::json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        );
    }
    let (name, arguments) = match step % 4 {
        0 => ("read_file", serde_json::json!({"path": "src/lib.rs"})),
        1 => ("search_text", serde_json::json!({"pattern": "fn item_7"})),
        2 => ("list_files", serde_json::json!({})),
        _ => (
            "write_file",
            serde_json::json!({"path": format!("out/step_{step}.txt"), "content": format!("step {step}\n")}),
        ),
    };
    format!(
        "{}{}data: [DONE]\n\n",
        frame(&serde_json::json!({"choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": format!("call-{step}"),
            "type": "function",
            "function": {"name": name, "arguments": arguments.to_string()}
        }]}, "finish_reason": null}]})),
        frame(&serde_json::json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}]})),
    )
}

/// One run: (spawn → first request, gaps between reply and next request,
/// last reply → exit).
fn fixture(temp: &Path) -> PathBuf {
    let project = temp.join("project");
    std::fs::create_dir_all(project.join("src")).expect("project");
    let mut source = String::new();
    for item in 0..400 {
        let _ = writeln!(source, "pub fn item_{item}() -> usize {{ {item} }}");
    }
    std::fs::write(project.join("src").join("lib.rs"), source).expect("fixture");
    for file in 0..200 {
        std::fs::write(
            project.join("src").join(format!("m{file}.rs")),
            format!("// module {file}\n"),
        )
        .expect("fixture");
    }
    // A repository, as a user's project is: the prompt reads its branch and
    // its changed files.
    for arguments in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.email=bench@example.invalid",
            "-c",
            "user.name=bench",
            "add",
            ".",
        ],
        &[
            "-c",
            "user.email=bench@example.invalid",
            "-c",
            "user.name=bench",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        let _ = Command::new("git")
            .args(arguments)
            .current_dir(&project)
            .output();
    }
    project
}

fn run(
    ha: &Path,
    temp: &Path,
    project: &Path,
    steps: usize,
) -> (Duration, Vec<Duration>, Duration, Duration) {
    let model_ms = number("--model-ms", 0) as u64;
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let replied: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&replied);
    std::thread::spawn(move || {
        let mut step = 0;
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let Some(request) = read_request(&mut socket) else {
                continue;
            };
            let arrived = Instant::now();
            // The model's own time, before its reply starts.
            std::thread::sleep(Duration::from_millis(model_ms));
            // Only the turn's own requests count; a side request (a title, a
            // summary) is answered with text and not timed.
            let is_turn = request["tools"]
                .as_array()
                .is_some_and(|tools| !tools.is_empty());
            let body = if is_turn {
                reply(step, steps)
            } else {
                reply(usize::MAX, 0)
            };
            let _ = socket.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = socket.flush();
            drop(socket);
            if is_turn {
                log.lock().expect("log").push((arrived, Instant::now()));
                step += 1;
            }
        }
    });
    let started = Instant::now();
    let output = Command::new(ha)
        .args(["exec", "benchmark the turn", "--approval", "full-auto"])
        .current_dir(project)
        .env("HA_HOME", temp.join("home"))
        .env(
            "HA_PROVIDER_ENDPOINT",
            format!("http://{address}/chat/completions"),
        )
        .env("HA_PROVIDER_MODEL", "fixture-model")
        .env("DEEPSEEK_API_KEY", "fixture-secret-value")
        .stdin(Stdio::null())
        .output()
        .expect("ha runs");
    let ended = Instant::now();
    assert!(
        output.status.success(),
        "ha exec failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        if line.starts_with("ha:") {
            eprintln!("{line}");
        }
    }
    let log = replied.lock().expect("log").clone();
    assert_eq!(log.len(), steps + 1, "every step reached the provider");
    let first = log[0].0 - started;
    let gaps = log.windows(2).map(|pair| pair[1].0 - pair[0].1).collect();
    let end = ended - log[log.len() - 1].1;
    // Everything but the model: the whole run less the time it was thinking.
    let model = Duration::from_millis(model_ms) * u32::try_from(log.len()).unwrap_or(u32::MAX);
    let host = (ended - started).saturating_sub(model);
    (first, gaps, end, host)
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn main() {
    let steps = number("--steps", 20);
    let runs = number("--runs", 3).max(1);
    let ha = arg("--ha").map_or_else(
        || {
            let exe = std::env::current_exe().expect("bench path");
            let release = exe.parent().and_then(Path::parent).expect("target dir");
            release.join(if cfg!(windows) { "ha.exe" } else { "ha" })
        },
        PathBuf::from,
    );
    // One home and one project for every run, as a user has: the first run
    // also creates the store and is reported on its own.
    let temp = tempfile::tempdir().expect("temp root");
    let temp_path = arg("--keep").map_or_else(|| temp.path().to_owned(), PathBuf::from);
    let project = fixture(&temp_path);
    let (first, _, end, _) = run(&ha, &temp_path, &project, steps);
    println!(
        "first run in a new home: start-up {:.0} ms, end {:.0} ms",
        ms(first),
        ms(end)
    );
    let mut firsts = Vec::new();
    let mut hosts = Vec::new();
    let mut gaps = Vec::new();
    let mut ends = Vec::new();
    for _ in 0..runs {
        let (first, run_gaps, end, host) = run(&ha, &temp_path, &project, steps);
        hosts.push(host);
        if arg("--each").is_some() {
            let each: Vec<String> = run_gaps
                .iter()
                .map(|gap| format!("{:.0}", ms(*gap)))
                .collect();
            println!(
                "start {:.0} ms, steps [{}], end {:.0} ms",
                ms(first),
                each.join(" "),
                ms(end)
            );
        }
        firsts.push(first);
        gaps.extend(run_gaps);
        ends.push(end);
    }
    let summary = |name: &str, mut times: Vec<Duration>| {
        times.sort();
        println!(
            "{name:<28} median {:>8.1} ms   max {:>8.1} ms",
            ms(times[times.len() / 2]),
            ms(times[times.len() - 1])
        );
    };
    println!("{runs} runs of {steps} tool steps through {}", ha.display());
    summary("start-up to first request", firsts);
    summary("host work per tool step", gaps);
    summary("last reply to exit", ends);
    summary("whole run less the model", hosts);
}
