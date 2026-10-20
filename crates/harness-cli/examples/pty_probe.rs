//! Drive `ha` in a pseudo-console and record what it paints, for replaying a
//! rendering bug frame by frame.
//!
//! `cargo run -p harness-cli --example pty_probe -- <out.bin> <ha.exe> <steps> -- <args...>`
//!
//! `steps` is a comma list run in order: `wait:<ms>`, `wheelup:<n>` (mouse wheel
//! notches at the window's middle), `pgup`, `end`, `key:<text>`, `enter`, `ctrlc`, `ctrls`,
//! `altenter`, `shiftenter`, `ctrlj`, `alt:<key>` (Alt and a key, as ESC then the
//! key), `settle:<ms>` (wait for the screen to go quiet and mark how long the
//! output went on), `until:<text>:<ms>` (wait until the app has written
//! `text` since the step began, at most `ms`; the mark says whether it did). The raw bytes
//! the app wrote go to `out.bin`; `out.bin.marks` lists the byte offset at which
//! each step began.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

#[allow(
    clippy::too_many_lines,
    reason = "one driver: the console, the reader, and every step it takes"
)]
fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let split = arguments.iter().position(|argument| argument == "--");
    let (head, tail) = match split {
        Some(index) => (&arguments[..index], &arguments[index + 1..]),
        None => (&arguments[..], &[][..]),
    };
    let [out, binary, steps] = head else {
        eprintln!("usage: pty_probe <out.bin> <ha.exe> <steps> -- <args...>");
        std::process::exit(2);
    };
    let (rows, cols) = (40_u16, 140_u16);
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("pty");
    let mut command = CommandBuilder::new(binary);
    for argument in tail {
        command.arg(argument);
    }
    command.cwd(std::env::current_dir().expect("cwd"));
    let mut child = pair.slave.spawn_command(command).expect("spawn");
    drop(pair.slave);
    let master = pair.master;
    let mut reader = master.try_clone_reader().expect("reader");
    let writer = Arc::new(Mutex::new(master.take_writer().expect("writer")));
    let transcript = Arc::new(Mutex::new(Vec::<u8>::new()));
    // When output last arrived and input was last sent: how far the screen
    // lags behind the input.
    let started = Instant::now();
    let last_output = Arc::new(Mutex::new(started));
    let last_input = Arc::new(Mutex::new(started));
    {
        let sink = Arc::clone(&transcript);
        let answer = Arc::clone(&writer);
        let arrived = Arc::clone(&last_output);
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            while let Ok(read) = reader.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                let bytes = &chunk[..read];
                sink.lock().expect("sink").extend_from_slice(bytes);
                *arrived.lock().expect("arrived") = Instant::now();
                if bytes.windows(4).any(|window| window == b"\x1b[6n") {
                    let _ = answer.lock().expect("w").write_all(b"\x1b[1;1R");
                }
            }
        });
    }
    let send = |bytes: &[u8]| {
        let mut writer = writer.lock().expect("writer");
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
        *last_input.lock().expect("input") = Instant::now();
    };
    let mut marks = String::new();
    for step in steps.split(',') {
        let offset = transcript.lock().expect("t").len();
        let _ = writeln!(marks, "{offset} {step}");
        let (name, value) = step.split_once(':').unwrap_or((step, ""));
        match name {
            "wait" => std::thread::sleep(Duration::from_millis(value.parse().unwrap_or(1000))),
            // Wheel notches 30 ms apart, or (`spinup`, `spindown`) 4 ms apart:
            // a wheel spun hard.
            "wheelup" | "wheeldown" | "spinup" | "spindown" => {
                let button = if name.ends_with("up") { 64 } else { 65 };
                let gap = if name.starts_with("spin") { 4 } else { 30 };
                for _ in 0..value.parse::<u32>().unwrap_or(1) {
                    send(format!("\x1b[<{button};{};{}M", cols / 2, rows / 2).as_bytes());
                    std::thread::sleep(Duration::from_millis(gap));
                }
            }
            // Wait until the screen has been quiet for 300 ms (at most the
            // value, in ms), and mark how long it went on painting after the
            // last input: how far the screen lags behind the wheel.
            "settle" => {
                let limit = Duration::from_millis(value.parse().unwrap_or(10_000));
                let begun = Instant::now();
                while begun.elapsed() < limit
                    && (begun.elapsed() < Duration::from_millis(500)
                        || last_output.lock().expect("output").elapsed()
                            < Duration::from_millis(300))
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                let output = *last_output.lock().expect("output");
                let input = *last_input.lock().expect("input");
                let _ = writeln!(
                    marks,
                    "{} settled {} ms after the input",
                    transcript.lock().expect("t").len(),
                    output.saturating_duration_since(input).as_millis()
                );
            }
            "pgup" => send(b"\x1b[5~"),
            "end" => send(b"\x1b[1;5F"),
            "key" => send(value.as_bytes()),
            "enter" => send(&[13]),
            "ctrlc" => send(&[3]),
            "ctrls" => send(&[19]),
            "altenter" => send(&[27, 13]),
            "shiftenter" => send(&[27, b'[', b'1', b'3', b';', b'2', b'u']),
            "ctrlj" => send(&[10]),
            "alt" => {
                let mut bytes = vec![27_u8];
                bytes.extend_from_slice(value.as_bytes());
                send(&bytes);
            }
            "until" => {
                let (text, limit) = value.rsplit_once(':').unwrap_or((value, "60000"));
                let limit = Duration::from_millis(limit.parse().unwrap_or(60_000));
                let begun = Instant::now();
                let found = loop {
                    let seen = transcript
                        .lock()
                        .expect("t")
                        .get(offset..)
                        .is_some_and(|written| {
                            written
                                .windows(text.len().max(1))
                                .any(|window| window == text.as_bytes())
                        });
                    if seen || begun.elapsed() >= limit {
                        break seen;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                let _ = writeln!(
                    marks,
                    "{} until {} {text} after {} ms",
                    transcript.lock().expect("t").len(),
                    if found { "found" } else { "missed" },
                    begun.elapsed().as_millis()
                );
            }
            _ => eprintln!("unknown step {step}"),
        }
    }
    std::fs::write(out, transcript.lock().expect("t").as_slice()).expect("write");
    std::fs::write(format!("{out}.marks"), marks).expect("marks");
    let _ = child.kill();
}
