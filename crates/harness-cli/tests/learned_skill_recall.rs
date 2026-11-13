//! `ha exec` recalls a learned skill as the app does (one function asks): the
//! request is the prompt, the skill goes in whole once per conversation, the
//! user is shown one line on stderr, and a recall counts an offer, not a use.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .current_dir(root)
        .env("HA_HOME", root)
        .env_remove("HA_MEMORY")
        .env_remove("HA_MEMORY_OFF")
        .stdin(Stdio::null())
        .args(args)
        .output()
        .expect("ha exec runs")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            stderr(output)
        )
    })
}

/// A learned skill in the user's global library, as `ha` writes one.
fn learned_skill(root: &Path, name: &str, description: &str, body: &str) {
    let directory = root.join("skills").join(name);
    std::fs::create_dir_all(&directory).expect("skill directory");
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
    )
    .expect("SKILL.md");
    std::fs::write(
        directory.join(".ha-learned.json"),
        r#"{"created_by": "ha", "created": "2026-10-01T00:00:00Z"}"#,
    )
    .expect("marker");
}

const OFFERED: &str = "learned skill db-migration offered for this request (unproven)";

/// `ha exec` used to recall no learned skill at all. It puts the skill that
/// fits the prompt in the run, tells the user on stderr, and says nothing the
/// next time in the same conversation, where the skill is already.
#[test]
fn exec_offers_the_skill_that_fits_once_per_conversation() {
    let root = tempfile::tempdir().expect("temporary home and project");
    learned_skill(
        root.path(),
        "db-migration",
        "Use when asked to run the database migration against staging.",
        "Write the database migration, check staging answers, then run it against staging.",
    );
    learned_skill(
        root.path(),
        "release-notes",
        "Use when the user asks for release notes or a changelog entry.",
        "Add the entry at the top of the changelog, one bullet per change.",
    );
    let prompt = "run the database migration against staging";
    let first = run(
        root.path(),
        &["exec", prompt, "--mock", "--output-format", "json"],
    );
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert!(stderr(&first).contains(OFFERED), "{}", stderr(&first));
    let first = json(&first);
    // The same conversation, the same request: the skill is already in it.
    let second = run(
        root.path(),
        &[
            "exec",
            "run the database migration against staging again",
            "--mock",
            "--continue",
            "--output-format",
            "json",
        ],
    );
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    assert_eq!(json(&second)["resumed_from"], first["session_id"]);
    assert!(
        !stderr(&second).contains("offered for this request"),
        "{}",
        stderr(&second)
    );
    // A different skill that fits a new request is offered in that conversation.
    let third = run(
        root.path(),
        &[
            "exec",
            "write the release notes for the changelog",
            "--mock",
            "--continue",
            "--output-format",
            "json",
        ],
    );
    assert!(
        stderr(&third).contains("learned skill release-notes offered for this request"),
        "{}",
        stderr(&third)
    );
    // A request no skill fits recalls nothing, and says nothing.
    let none = run(
        root.path(),
        &[
            "exec",
            "sing me a song of autumn",
            "--mock",
            "--output-format",
            "json",
        ],
    );
    assert!(
        !stderr(&none).contains("offered for this request"),
        "{}",
        stderr(&none)
    );
    // Counted as offered, never as used: two offers, no activation.
    let usage = std::fs::read_to_string(
        root.path()
            .join("data")
            .join("skill-usage")
            .join("global.json"),
    )
    .expect("the usage record");
    let usage: serde_json::Value = serde_json::from_str(&usage).expect("usage JSON");
    let migration = &usage["skills"]["db-migration"];
    assert_eq!(migration["offered"], 1, "{usage}");
    assert!(
        migration["use"].is_null() || migration["use"] == 0,
        "{usage}"
    );
}
