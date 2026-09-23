//! The M12 capability-probe fixture: one process that can be asked to do
//! exactly the thing a boundary is supposed to stop.
//!
//! It is a fixture, not a product surface: `ha` never calls it, and it holds no
//! authority. Each mode makes one *observable* attempt — tick a file from a
//! detached descendant, read a canary, write a canary, connect to a loopback
//! listener, open a credential pipe, allocate memory, spawn a crowd — so a probe
//! can decide a capability by looking at the effect from the outside instead of
//! asking this process what happened to it.
//!
//! Exit codes: 0 the attempt succeeded, 3 the attempt was refused by the
//! platform, 2 the arguments were wrong.

use std::{
    env,
    io::{self, Write},
    net::TcpStream,
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const TICK_INTERVAL: Duration = Duration::from_millis(100);
const REFUSED: i32 = 3;
const USAGE: i32 = 2;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(mode) = args.first().map(String::as_str) else {
        eprintln!("usage: m12_probe_child <mode> [args]");
        std::process::exit(USAGE);
    };
    let code = match mode {
        "tick" => tick(&args),
        "ticks" => ticks(&args, false),
        "spawn-detach" => ticks(&args, true),
        "spawn-count" => spawn_count(&args),
        "env" => dump_environment(),
        "read" => read_file(&args),
        "write" => write_file(&args),
        "connect" => connect(&args),
        "pipe" => pipe(&args),
        "alloc" => alloc(&args),
        "sleep" => sleep_ms(&args),
        "flood" => flood(&args),
        other => {
            eprintln!("unknown probe mode {other}");
            USAGE
        }
    };
    std::process::exit(code);
}

/// Append one line every 100 ms until the lifetime runs out.
fn tick(args: &[String]) -> i32 {
    let (Some(path), Some(lifetime)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    let Ok(lifetime) = lifetime.parse::<u64>() else {
        return USAGE;
    };
    tick_until(&PathBuf::from(path), lifetime)
}

fn tick_until(path: &std::path::Path, lifetime_ms: u64) -> i32 {
    let deadline = std::time::Instant::now() + Duration::from_millis(lifetime_ms);
    while std::time::Instant::now() < deadline {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "t");
            }
            // A tick that cannot be written is not a boundary: report it and
            // stop rather than spin.
            Err(error) => {
                eprintln!("tick failed: {error}");
                return REFUSED;
            }
        }
        thread::sleep(TICK_INTERVAL);
    }
    0
}

/// Spawn detached descendants that tick the same file.
///
/// With `detach` the parent exits immediately afterwards: that is the shape
/// which catches a runner reporting a run as complete while the tree it started
/// is still writing.
fn ticks(args: &[String], detach: bool) -> i32 {
    let (Some(path), Some(descendants), Some(lifetime)) = (args.get(1), args.get(2), args.get(3))
    else {
        return USAGE;
    };
    let Ok(descendants) = descendants.parse::<u32>() else {
        return USAGE;
    };
    let Ok(lifetime) = lifetime.parse::<u64>() else {
        return USAGE;
    };
    let Ok(executable) = env::current_exe() else {
        return REFUSED;
    };
    let mut spawned = 0_u32;
    let mut pids = Vec::new();
    for _ in 0..descendants {
        let child = Command::new(&executable)
            .args(["tick", path, &lifetime.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match child {
            Ok(child) => {
                spawned += 1;
                pids.push(child.id());
            }
            Err(error) => eprintln!("spawn failed: {error}"),
        }
    }
    // The pids are written down so a control that deliberately lets the tree
    // survive can bound the leak instead of leaving it to the lifetime.
    let pid_file = format!("{path}.pids");
    if !pids.is_empty() {
        let _ = std::fs::write(
            &pid_file,
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    println!("spawned={spawned}");
    let _ = io::stdout().flush();
    if detach {
        return 0;
    }
    tick_until(&PathBuf::from(path), lifetime)
}

/// Spawn a crowd, report how many were still running, then stay alive.
fn spawn_count(args: &[String]) -> i32 {
    let (Some(count), Some(lifetime)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    let Ok(count) = count.parse::<u32>() else {
        return USAGE;
    };
    let Ok(lifetime) = lifetime.parse::<u64>() else {
        return USAGE;
    };
    let Ok(executable) = env::current_exe() else {
        return REFUSED;
    };
    let mut children = Vec::new();
    for index in 0..count {
        // Relative on purpose: the runner's working directory is the run root,
        // so a crowd's own files stay inside the fixture directory.
        let file = format!("crowd-{}-{index}.txt", std::process::id());
        if let Ok(child) = Command::new(&executable)
            .args(["tick", &file, &lifetime.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            children.push(child);
        }
    }
    thread::sleep(Duration::from_millis(500));
    let mut alive = 0_usize;
    for child in &mut children {
        if matches!(child.try_wait(), Ok(None)) {
            alive += 1;
        }
    }
    println!("spawned={} alive={alive}", children.len());
    let _ = io::stdout().flush();
    thread::sleep(Duration::from_millis(lifetime));
    0
}

fn dump_environment() -> i32 {
    let mut names: Vec<(String, String)> = env::vars().collect();
    names.sort();
    for (name, value) in names {
        println!("{name}={value}");
    }
    0
}

fn read_file(args: &[String]) -> i32 {
    let Some(path) = args.get(1) else {
        return USAGE;
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            println!("read={text}");
            0
        }
        Err(error) => {
            println!("error={error}");
            REFUSED
        }
    }
}

fn write_file(args: &[String]) -> i32 {
    let (Some(path), Some(text)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    match std::fs::write(path, text) {
        Ok(()) => {
            println!("wrote={path}");
            0
        }
        Err(error) => {
            println!("error={error}");
            REFUSED
        }
    }
}

fn connect(args: &[String]) -> i32 {
    let (Some(port), Some(nonce)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    let Ok(port) = port.parse::<u16>() else {
        return USAGE;
    };
    match TcpStream::connect(("127.0.0.1", port)) {
        Ok(mut stream) => {
            let sent = stream.write_all(nonce.as_bytes()).is_ok();
            println!("connected={sent}");
            if sent { 0 } else { REFUSED }
        }
        Err(error) => {
            println!("error={error}");
            REFUSED
        }
    }
}

#[cfg(windows)]
fn pipe(args: &[String]) -> i32 {
    let (Some(address), Some(token)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    // A Windows named pipe is opened by path, so the same std file API the
    // fixture uses for a canary opens a credential pipe too.
    match std::fs::OpenOptions::new().write(true).open(address) {
        Ok(mut handle) => {
            let sent = handle.write_all(token.as_bytes()).is_ok();
            println!("sent={sent}");
            if sent { 0 } else { REFUSED }
        }
        Err(error) => {
            println!("error={error}");
            REFUSED
        }
    }
}

#[cfg(unix)]
fn pipe(args: &[String]) -> i32 {
    use std::os::unix::net::UnixStream;

    let (Some(address), Some(token)) = (args.get(1), args.get(2)) else {
        return USAGE;
    };
    match UnixStream::connect(address) {
        Ok(mut stream) => {
            let sent = stream.write_all(token.as_bytes()).is_ok();
            println!("sent={sent}");
            if sent { 0 } else { REFUSED }
        }
        Err(error) => {
            println!("error={error}");
            REFUSED
        }
    }
}

fn alloc(args: &[String]) -> i32 {
    let Some(mib) = args.get(1) else {
        return USAGE;
    };
    let Ok(mib) = mib.parse::<usize>() else {
        return USAGE;
    };
    let bytes = mib.saturating_mul(1024 * 1024);
    let mut buffer = vec![0_u8; bytes];
    // Touch one byte per page: an untouched allocation would be a reservation,
    // not memory this process actually holds.
    for index in (0..bytes).step_by(4096) {
        buffer[index] = 1;
    }
    println!("allocated={mib} bytes={bytes}");
    let _ = io::stdout().flush();
    // Keep the memory resident until the probe is done with this process.
    thread::sleep(Duration::from_millis(200));
    drop(buffer);
    0
}

fn sleep_ms(args: &[String]) -> i32 {
    let Some(milliseconds) = args.get(1) else {
        return USAGE;
    };
    let Ok(milliseconds) = milliseconds.parse::<u64>() else {
        return USAGE;
    };
    thread::sleep(Duration::from_millis(milliseconds));
    0
}

fn flood(args: &[String]) -> i32 {
    let Some(bytes) = args.get(1) else {
        return USAGE;
    };
    let Ok(bytes) = bytes.parse::<usize>() else {
        return USAGE;
    };
    let chunk = vec![b'x'; 8 * 1024];
    let mut written = 0_usize;
    let mut stdout = io::stdout();
    while written < bytes {
        let take = chunk.len().min(bytes - written);
        if stdout.write_all(&chunk[..take]).is_err() {
            return REFUSED;
        }
        written += take;
    }
    let _ = stdout.flush();
    0
}
