//! A small, deterministic external plugin used as a P6 fixture.
//!
//! It implements the real protocol: it shakes hands before doing any work, then
//! answers tool calls. Behaviour is switched with environment variables so the
//! acceptance target can reproduce hostile conditions without shipping hostile
//! code paths into production flows.
//!
//! Switches (all optional):
//! - `P6_FIXTURE_MODE`: `normal` (default) | `malformed_frame` | `oversize_frame`
//!   | `duplicate_id` | `flood_stderr` | `ignore_cancel` | `bad_protocol` |
//!   `unknown_capability` | `denied_host_method` | `exit_before_handshake`
//!   | `crash_after_effect` | `noisy_stderr` | `honour_cancel`
//!   | `crash_mid_call` | `flood_stdout` | `malformed_after_handshake`
//! - `P6_FIXTURE_SECRET`: if set, the plugin echoes only whether the variable
//!   exists, never its value. It exists to prove the host does not leak one.

use std::io::{BufRead, Write};

#[allow(clippy::too_many_lines)] // One deterministic fixture loop; splitting hides the mode table.
fn main() {
    let mode = std::env::var("P6_FIXTURE_MODE").unwrap_or_else(|_| "normal".to_owned());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    if mode == "exit_before_handshake" {
        std::process::exit(3);
    }

    if mode == "noisy_stderr" {
        let _ = writeln!(
            std::io::stderr(),
            "fixture diagnostics on stderr must never be parsed as protocol"
        );
    }

    let mut handshake_done = false;
    // Call ids this fixture has deliberately left unanswered so a later cancel
    // can settle them.
    let mut parked: Vec<String> = Vec::new();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
            // A malformed request is answered with a protocol error frame when
            // the id can be recovered, and ignored otherwise.
            continue;
        };
        let id = frame
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_owned();
        let method = frame
            .get("method")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_owned();
        let kind = frame
            .get("kind")
            .and_then(|value| value.as_str())
            .unwrap_or("message")
            .to_owned();

        // A host cancel is answered only when this fixture is told to honour it;
        // otherwise the fixture keeps working, which is what the host's grace
        // window and process-tree termination exist for.
        if kind == "cancel" {
            if mode == "honour_cancel" && parked.iter().any(|parked| parked == &id) {
                parked.retain(|parked| parked != &id);
                let reply = serde_json::json!({
                    "protocol_version": 1,
                    "id": id,
                    "kind": "error",
                    "payload": {"code": "canceled", "message": "fixture stopped on request"},
                });
                let _ = writeln!(stdout, "{reply}");
                let _ = stdout.flush();
            }
            continue;
        }

        if method == "handshake" {
            if mode == "crash_after_effect" && std::env::var("P6_FIXTURE_CRASH_FIRST").is_ok() {
                std::process::exit(9);
            }
            if mode == "malformed_frame" {
                let _ = writeln!(stdout, "{{ this is not json");
                let _ = stdout.flush();
                continue;
            }
            if mode == "oversize_frame" {
                let filler = "x".repeat(2 * 1024 * 1024);
                let _ = writeln!(stdout, "{{\"id\":\"{id}\",\"filler\":\"{filler}\"}}");
                let _ = stdout.flush();
                continue;
            }
            let protocol = if mode == "bad_protocol" { 99 } else { 1 };
            let capability = if mode == "unknown_capability" {
                "not_a_capability"
            } else {
                "tools"
            };
            let host_methods = if mode == "denied_host_method" {
                vec!["host.dangerous.unrestricted"]
            } else {
                vec!["host.echo"]
            };
            let reply = serde_json::json!({
                "protocol_version": protocol,
                "id": id,
                "kind": "response",
                "payload": {
                    "protocol_version": protocol,
                    "plugin_id": "p6.fixture.tools",
                    "implementation_version": "0.1.0",
                    "executable_digest": executable_digest(),
                    "capabilities": [{"capability": capability, "api_version": 1}],
                    "host_methods": host_methods,
                    "config_schema_version": 1,
                },
            });
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
            handshake_done = true;
            continue;
        }

        if !handshake_done {
            // No work is admitted before a handshake.
            let reply = serde_json::json!({
                "protocol_version": 1,
                "id": id,
                "kind": "error",
                "payload": {"code": "no_handshake", "message": "handshake required"},
            });
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
            continue;
        }

        if mode == "duplicate_id" {
            // Answer twice with the same id: the second frame must not settle
            // anything.
            for _ in 0..2 {
                let reply = serde_json::json!({
                    "protocol_version": 1,
                    "id": id,
                    "kind": "response",
                    "payload": {"echo": method, "duplicate": true},
                });
                let _ = writeln!(stdout, "{reply}");
            }
            let _ = stdout.flush();
            continue;
        }

        if mode == "ignore_cancel" {
            // Never answer: the host must expire the deadline and terminate us.
            std::thread::sleep(std::time::Duration::from_secs(30));
            continue;
        }

        if mode == "honour_cancel" {
            // Park the call instead of answering it. A later cancel for this id
            // is what settles it, which is exactly the contract under test.
            parked.push(id);
            continue;
        }

        if mode == "malformed_after_handshake" {
            // Handshake succeeded, then the protocol channel goes bad. The host
            // must drop the garbage framed as data and still bound the call.
            let _ = writeln!(stdout, "{{ this is not json");
            let _ = writeln!(stdout, "{{\"protocol_version\":1,\"id\":\"{id}\"");
            let _ = stdout.flush();
            let reply = serde_json::json!({
                "protocol_version": 1,
                "id": id,
                "kind": "response",
                "payload": {"survived": "malformed_after_handshake"},
            });
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
            continue;
        }

        if mode == "crash_mid_call" {
            // Die without answering: every pending call must settle as
            // uncertain, and the handle must be invalid afterwards.
            std::process::exit(11);
        }

        if mode == "flood_stdout" {
            // Flood the protocol channel with frames the host never asked for.
            // The host must drop them without growing a buffer and without
            // settling anything.
            for index in 0..20_000 {
                let filler = "f".repeat(64);
                let noise = serde_json::json!({
                    "protocol_version": 1,
                    "id": format!("flood-{index}"),
                    "kind": "response",
                    "payload": {"filler": filler},
                });
                let _ = writeln!(stdout, "{noise}");
            }
            let _ = stdout.flush();
            let reply = serde_json::json!({
                "protocol_version": 1,
                "id": id,
                "kind": "response",
                "payload": {"survived": "flood"},
            });
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
            continue;
        }

        if mode == "flood_stderr" {
            let filler = "e".repeat(4096);
            for _ in 0..512 {
                let _ = writeln!(std::io::stderr(), "{filler}");
            }
        }

        if mode == "crash_after_effect" {
            // Report the effect, then die: the host must treat the settlement as
            // uncertain and never retry blindly.
            let reply = serde_json::json!({
                "protocol_version": 1,
                "id": id,
                "kind": "response",
                "payload": {"effect": "applied", "note": "crashing after the effect"},
            });
            let _ = writeln!(stdout, "{reply}");
            let _ = stdout.flush();
            std::process::exit(9);
        }

        let payload = handle_tool(&method, &frame);
        let reply = serde_json::json!({
            "protocol_version": 1,
            "id": id,
            "kind": "response",
            "payload": payload,
        });
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}

fn handle_tool(method: &str, frame: &serde_json::Value) -> serde_json::Value {
    let arguments = frame
        .get("payload")
        .and_then(|value| value.get("arguments"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match method {
        "tool.read_observation" => serde_json::json!({
            "observation": "fixture observation",
            "arguments": arguments,
        }),
        "tool.write_note" => serde_json::json!({
            "effect": "applied",
            "arguments": arguments,
            "secret_present": std::env::var("P6_FIXTURE_SECRET").is_ok(),
        }),
        "env.names" => {
            let mut names = std::env::vars().map(|(name, _)| name).collect::<Vec<_>>();
            names.sort();
            serde_json::json!({ "names": names })
        }
        "provider.describe" => serde_json::json!({
            "provider_id": "p6.fixture.provider",
            "model": "fixture-model-1",
            "streaming": true,
        }),
        "provider.stream" => serde_json::json!({
            "model": "fixture-model-1",
            "events": [
                {"kind": "started"},
                {"kind": "text_delta", "text": "fixture response"},
                {"kind": "completed", "stop_reason": "stop"}
            ],
            "usage": {"input_tokens": 3, "output_tokens": 2}
        }),
        other => serde_json::json!({"error": format!("unknown method {other}")}),
    }
}

/// The plugin reports its own digest, which must match the manifest exactly.
fn executable_digest() -> String {
    use sha2::{Digest, Sha256};
    let Ok(path) = std::env::current_exe() else {
        return "sha256:unknown".to_owned();
    };
    let Ok(bytes) = std::fs::read(path) else {
        return "sha256:unknown".to_owned();
    };
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("sha256:{}", hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}
