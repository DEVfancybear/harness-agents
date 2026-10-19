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
fn a_bug_report_holds_the_problems_and_no_secret() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path();
    let now = chrono::Utc::now();
    let stamp = now.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    seed_worker_and_credentials(&home.join("data"), &stamp);

    // A real process leaves its records: `ha diag path` at debug level.
    let path = ha(home, &["diag", "path"]);
    assert!(
        path.status.success(),
        "{}",
        String::from_utf8_lossy(&path.stderr)
    );
    let diagnostics = String::from_utf8(path.stdout).unwrap();
    assert!(diagnostics.trim().ends_with("diagnostics"), "{diagnostics}");
    // And one that said a secret, the way an error message might.
    std::fs::write(
        Path::new(diagnostics.trim()).join("20260101-tui-7.jsonl"),
        format!(
            "{}\n{}\n",
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
}
