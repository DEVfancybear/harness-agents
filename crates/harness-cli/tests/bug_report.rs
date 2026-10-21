//! `ha bug-report` through the compiled binary: one zip holding what ha
//! recorded, with the problems in front and no secret in it.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output};

/// A value no pattern rule knows: only the known-secret pass can find it.
const SECRET: &str = "plainvalue-7Q2W9E4R1T";

fn ha(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(args)
        .current_dir(home)
        .env("HA_HOME", home)
        .env("HA_OFFLINE", "1")
        .env("HA_DIAG", "debug")
        .env("DEEPSEEK_API_KEY", SECRET)
        .env_remove("HA_DIAG_DIR")
        .env_remove("HA_CREDENTIALS_DIR")
        .output()
        .expect("run ha")
}

/// The entries of a zip with deflated entries, as `ha bug-report` writes it.
fn entries(bytes: &[u8]) -> Vec<(String, String)> {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]));
    let u32_at = |at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let mut entries = Vec::new();
    let mut at = 0;
    while u32_at(at) == 0x0403_4b50 {
        let compressed = u32_at(at + 18);
        let name_len = u16_at(at + 26);
        let extra = u16_at(at + 28);
        let name = String::from_utf8(bytes[at + 30..at + 30 + name_len].to_vec()).unwrap();
        let start = at + 30 + name_len + extra;
        let mut text = String::new();
        flate2::read::DeflateDecoder::new(&bytes[start..start + compressed])
            .read_to_string(&mut text)
            .expect("a deflated UTF-8 entry");
        entries.push((name, text));
        at = start + compressed;
    }
    entries
}

/// A worker log with a panic as the default hook reports it (no time), a
/// worker descriptor with its token, and a credential file.
fn seed_worker_and_credentials(data: &Path, stamp: &str) {
    // A worker that panicked, as the default hook reports it: no time.
    let workers = data.join("workers");
    std::fs::create_dir_all(&workers).unwrap();
    std::fs::write(
        workers.join("0123456789abcdef.log"),
        format!(
            "{stamp} worker 1 serving C:\\p on 127.0.0.1:1\n\
             thread 'main' (42) panicked at crates\\harness-cli\\src\\interactive\\agents\\worker.rs:492:22:\n\
             Cannot start a runtime from within a runtime.\n\
             {stamp} the worker ended (exit code: 101); key {SECRET} in a line\n"
        ),
    )
    .unwrap();
    std::fs::write(
        workers.join("0123456789abcdef.json"),
        r#"{"pid":1,"port":1,"token":"descriptor-token-value"}"#,
    )
    .unwrap();
    // Credentials are never read into the report.
    let private = data.join("private");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(
        private.join("auth.json"),
        r#"{"deepseek":"private-file-value"}"#,
    )
    .unwrap();
}

#[test]
#[allow(clippy::too_many_lines, reason = "one report, read back file by file")]
fn a_bug_report_holds_the_problems_and_no_secret() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let now = chrono::Utc::now();
    let stamp = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    seed_worker_and_credentials(&home.join("data"), &stamp);

    // A process that only starts and exits leaves no file, as no state.
    let path = ha(home, &["diag", "path"]);
    assert!(
        path.status.success(),
        "{}",
        String::from_utf8_lossy(&path.stderr)
    );
    let diagnostics = String::from_utf8(path.stdout).unwrap();
    assert!(diagnostics.trim().ends_with("diagnostics"), "{diagnostics}");
    assert!(
        !Path::new(diagnostics.trim()).exists(),
        "ha diag path wrote a log"
    );
    // A process that said a secret, the way an error message might, and a
    // record whose millisecond time passes the card-number check: redacting
    // the report's lines as text turned such a time into a card and the line
    // into something that is not JSON.
    std::fs::create_dir_all(diagnostics.trim()).unwrap();
    let mut card_like = now.timestamp_millis();
    while !luhn(card_like) {
        card_like += 1;
    }
    std::fs::write(
        Path::new(diagnostics.trim()).join("20260101-tui-7.jsonl"),
        format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"v":1,"ms":card_like,"ts":stamp,"pid":7,"role":"tui","level":"warn","kind":"provider","event":"stream_restarted","task":"task_01a13d6f-385f-7390-a32a-c2a3326523b3"}),
            serde_json::json!({"v":1,"ms":now.timestamp_millis(),"ts":stamp,"pid":7,"role":"tui","level":"info","kind":"process","event":"start"}),
            serde_json::json!({"v":1,"ms":now.timestamp_millis(),"ts":stamp,"pid":7,"role":"tui","level":"error","kind":"store","event":"open_failed","msg":format!("refused with {SECRET}"),"task":"task_01a13d6f-385f-7390-a32a-c2a3326523b3"}),
        ),
    )
    .unwrap();

    let out = home.join("report.zip");
    let report = ha(
        home,
        &[
            "bug-report",
            "--since",
            "1h",
            "--out",
            out.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let summary: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert!(summary["problems"].as_u64().unwrap() >= 2, "{summary}");

    let entries = entries(&std::fs::read(&out).unwrap());
    let names = entries
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names[0], "SUMMARY.md", "{names:?}");
    assert!(names.contains(&"workers/0123456789abcdef.log"), "{names:?}");
    assert!(names.contains(&"env.json"), "{names:?}");
    assert!(
        names.iter().any(|name| name.starts_with("diagnostics/")),
        "{names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("private")),
        "{names:?}"
    );

    let summary_md = &entries[0].1;
    assert!(summary_md.contains("## Problems"), "{summary_md}");
    assert!(
        summary_md.contains("worker.rs:492:22"),
        "the panic is named: {summary_md}"
    );
    assert!(
        summary_md.contains("Processes without an exit record"),
        "a process that started and never ended is named: {summary_md}"
    );
    assert!(summary_md.contains("store.open_failed"), "{summary_md}");
    for (name, text) in &entries {
        assert!(!text.contains(SECRET), "{name} holds the secret");
        assert!(
            !text.contains("descriptor-token-value"),
            "{name} holds the token"
        );
        assert!(
            !text.contains("private-file-value"),
            "{name} holds a credential"
        );
    }
    assert!(
        entries
            .iter()
            .any(|(_, text)| text.contains("[REDACTED:known]")),
        "the secret was replaced, not dropped with its line"
    );
    for (name, text) in &entries {
        assert!(
            !text.contains("REDACTED:pii:credit_card"),
            "{name} took a time for a card"
        );
    }
    let timeline = &entries
        .iter()
        .find(|(name, _)| name == "timeline.jsonl")
        .expect("timeline")
        .1;
    for line in timeline.lines() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|error| panic!("a timeline line is not JSON ({error}): {line}"));
    }
    assert!(
        timeline.contains(&format!("\"ms\":{card_like}")),
        "{timeline}"
    );
    for (name, text) in entries.iter().filter(|(name, _)| {
        name.starts_with("store/")
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            && name.matches('/').count() == 1
    }) {
        serde_json::from_str::<serde_json::Value>(text)
            .unwrap_or_else(|error| panic!("{name} is not JSON: {error}"));
    }
}

/// A conversation's report holds its delegated children: their ledger, a
/// summary table saying which failed and why, and their own diagnostics -
/// which a report for the conversation used to drop, so a `[child-failed]`
/// notice was all there was to go on.
#[test]
fn a_conversations_report_holds_its_children_and_why_they_failed() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let now = chrono::Utc::now();
    let stamp = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let data = home.join("data");
    let parent = "task_01a13d6f-385f-7390-a32a-c2a3326523b3";
    let child = "task_01a15a52-0041-7773-a344-df763247cd4c";
    let ledger = data.join("delegation").join("ledger");
    std::fs::create_dir_all(&ledger).unwrap();
    std::fs::write(
        ledger.join(format!("{parent}.json")),
        serde_json::json!([{
            "task_id": child, "name": "coder-f81dea3a", "role": "coder",
            "model": "openai-codex/gpt-5.4", "depth": 1, "parent": null,
            "status": "failed", "error": "rate_limited: You have hit your usage limit",
            "duration_ms": 4200, "tool_calls": 3, "answer": null
        }])
        .to_string(),
    )
    .unwrap();
    let diagnostics = data.join("diagnostics");
    std::fs::create_dir_all(&diagnostics).unwrap();
    std::fs::write(
        diagnostics.join("20260101-worker-9.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"v":1,"ms":now.timestamp_millis(),"ts":stamp,"pid":9,"role":"worker","level":"info","kind":"turn","event":"start","task":parent}),
            serde_json::json!({"v":1,"ms":now.timestamp_millis(),"ts":stamp,"pid":9,"role":"worker","level":"warn","kind":"provider","event":"attempt_failed","msg":"rate_limited: the child's own limit","task":child}),
        ),
    )
    .unwrap();
    let out = home.join("child.zip");
    let report = ha(
        home,
        &[
            "bug-report",
            "--since",
            "1h",
            "--task",
            parent,
            "--out",
            out.to_str().unwrap(),
        ],
    );
    assert!(
        report.status.success(),
        "{}",
        String::from_utf8_lossy(&report.stderr)
    );
    let entries = entries(&std::fs::read(&out).unwrap());
    let summary = &entries[0].1;
    assert!(summary.contains("## Sub-agents"), "{summary}");
    assert!(summary.contains("coder-f81dea3a"), "{summary}");
    assert!(summary.contains("openai-codex/gpt-5.4"), "{summary}");
    assert!(
        summary.contains("You have hit your usage limit"),
        "{summary}"
    );
    assert!(
        entries
            .iter()
            .any(|(name, _)| name == &format!("delegation/ledger/{parent}.json")),
        "the ledger is in the report"
    );
    assert!(
        entries
            .iter()
            .any(|(name, text)| name.starts_with("diagnostics/")
                && text.contains("the child's own limit")),
        "the child's records are kept with the conversation's"
    );
}

/// Whether a number passes the card-number (Luhn) check.
fn luhn(number: i64) -> bool {
    let digits = number.to_string();
    let sum: u32 = digits
        .chars()
        .rev()
        .filter_map(|digit| digit.to_digit(10))
        .enumerate()
        .map(|(index, digit)| {
            if index % 2 == 1 {
                let doubled = digit * 2;
                if doubled > 9 { doubled - 9 } else { doubled }
            } else {
                digit
            }
        })
        .sum();
    sum.is_multiple_of(10)
}
