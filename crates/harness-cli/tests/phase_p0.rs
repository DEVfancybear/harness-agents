use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use harness_types::{
    ContentHash, ErrorCode, EventEnvelope, ProjectId, canonical_json_bytes,
    generated_schema_documents, validate_schema_version, verify_continuation_fixture,
};
use serde_json::Value;

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture_path(relative: &str) -> PathBuf {
    repository_root().join("tests/fixtures").join(relative)
}

fn run_ha(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ha"))
        .args(arguments)
        .output()
        .expect("compiled ha binary should execute")
}

fn output_text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("CLI output must be UTF-8")
}

#[test]
fn p0_f01_cli_help_and_version_run_without_credentials() {
    let help = run_ha(&["--help"]);
    assert!(help.status.success());
    assert!(output_text(&help.stdout).contains("Personal coding-agent harness."));
    assert!(output_text(&help.stderr).is_empty());

    let version = run_ha(&["--version"]);
    assert!(version.status.success());
    assert!(output_text(&version.stdout).starts_with("ha 0.1.0"));
    assert!(output_text(&version.stderr).is_empty());
}

#[test]
fn p0_f02_cli_rejects_unknown_config_and_option_inputs() {
    let valid_path = fixture_path("p0/config/valid.toml");
    let valid = run_ha(&[
        "config",
        "validate",
        "--config",
        valid_path.to_str().expect("fixture path must be UTF-8"),
        "--json",
    ]);
    assert!(valid.status.success());
    let valid_json: Value =
        serde_json::from_slice(&valid.stdout).expect("valid output must be JSON");
    assert_eq!(valid_json["schema_version"], 1);
    assert_eq!(valid_json["valid"], true);
    assert!(output_text(&valid.stderr).is_empty());

    let unknown_path = fixture_path("p0/config/unknown-field.toml");
    let unknown = run_ha(&[
        "config",
        "validate",
        "--config",
        unknown_path.to_str().expect("fixture path must be UTF-8"),
    ]);
    assert!(!unknown.status.success());
    assert!(output_text(&unknown.stdout).is_empty());
    assert!(output_text(&unknown.stderr).starts_with("config_unknown_field:"));

    let unsupported_path = fixture_path("p0/config/unsupported-schema.toml");
    let unsupported = run_ha(&[
        "config",
        "validate",
        "--config",
        unsupported_path
            .to_str()
            .expect("fixture path must be UTF-8"),
    ]);
    assert!(!unsupported.status.success());
    assert!(output_text(&unsupported.stdout).is_empty());
    assert!(output_text(&unsupported.stderr).starts_with("unsupported_schema_version:"));

    let option_error = run_ha(&["--not-a-real-option"]);
    assert!(!option_error.status.success());
    assert!(output_text(&option_error.stdout).is_empty());
    assert!(output_text(&option_error.stderr).contains("unexpected argument"));
}

#[test]
fn p0_f03_contract_schemas_are_generated_from_real_types() {
    let schema_root = repository_root().join("schemas");
    let attributes = fs::read_to_string(repository_root().join(".gitattributes"))
        .expect("schema line-ending policy must exist");
    assert!(
        attributes.contains("schemas/*.json text eol=lf"),
        "generated schemas must retain LF in every checkout"
    );
    let documents = generated_schema_documents();
    assert_eq!(documents.len(), 9);
    for document in documents {
        let mut expected =
            serde_json::to_string_pretty(&document.value).expect("schema serializes");
        expected.push('\n');
        let committed = fs::read_to_string(schema_root.join(document.file_name))
            .expect("committed schema must exist");
        assert_eq!(committed, expected, "schema drift: {}", document.file_name);
        assert_eq!(document.value["x-harness-schema-version"], 1);
    }
}

#[test]
fn p0_f04_contracts_reject_invalid_boundaries() {
    assert_eq!(
        ProjectId::parse("project_00000000-0000-0000-0000-000000000000")
            .expect_err("nil ID must be invalid")
            .code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        ProjectId::parse("task_018f8b64-5c8d-7a0a-8f21-123456789abc")
            .expect_err("wrong prefix must be invalid")
            .code(),
        ErrorCode::InvalidId
    );
    assert_eq!(
        validate_schema_version(2)
            .expect_err("unknown schema must be invalid")
            .code(),
        ErrorCode::UnsupportedSchemaVersion
    );
    assert_eq!(
        EventEnvelope::parse_json(r#"{"schema_version":1}"#)
            .expect_err("authority must be explicit")
            .code(),
        ErrorCode::MissingAuthority
    );
    assert_eq!(
        ContentHash::parse("sha256:ABCDEF")
            .expect_err("uppercase or short hash must be invalid")
            .code(),
        ErrorCode::InvalidHash
    );
}

#[test]
fn p0_f05_canonical_hash_is_deterministic_and_rejects_float() {
    let first: Value =
        serde_json::from_str(r#"{"z":2,"a":{"y":1,"x":0}}"#).expect("test JSON parses");
    let reordered: Value =
        serde_json::from_str(r#"{"a":{"x":0,"y":1},"z":2}"#).expect("test JSON parses");
    let changed: Value =
        serde_json::from_str(r#"{"a":{"x":0,"y":1},"z":3}"#).expect("test JSON parses");
    assert_eq!(
        canonical_json_bytes(&first).expect("canonical JSON"),
        canonical_json_bytes(&reordered).expect("canonical JSON")
    );
    assert_eq!(
        ContentHash::from_canonical_json(&first).expect("canonical JSON hash"),
        ContentHash::from_canonical_json(&reordered).expect("canonical JSON hash")
    );
    assert_ne!(
        ContentHash::from_canonical_json(&first).expect("canonical JSON hash"),
        ContentHash::from_canonical_json(&changed).expect("canonical JSON hash")
    );
    let float: Value = serde_json::from_str(r#"{"value":1.5}"#).expect("test JSON parses");
    assert_eq!(
        canonical_json_bytes(&float)
            .expect_err("P0 float must be rejected")
            .code(),
        ErrorCode::InvalidPayload
    );
}

#[test]
fn p0_f06_continuation_fixture_is_complete_without_a_projector() {
    let report = verify_continuation_fixture(&fixture_path("p0/continuation"))
        .expect("static continuation fixture must be valid");
    assert_eq!(report.event_count, 10);
    assert_eq!(report.completed_steps, ["plan-inspect", "plan-test-a"]);
    assert_eq!(report.failed_check_count, 1);
    assert_eq!(report.pending_steps, ["plan-fix-b"]);
    assert_eq!(report.current_decision, "B");
    assert!(report.next_action.contains("parser_b"));
}

#[test]
fn p0_f07_phase_gate_self_test_exercises_negative_controls() {
    let root = repository_root();
    let output = Command::new("pwsh")
        .current_dir(&root)
        .args([
            "-NoProfile",
            "-File",
            "scripts/Verify-Phase.ps1",
            "-Phase",
            "P0",
            "-SelfTest",
        ])
        .output()
        .expect("PowerShell 7 must execute the phase gate self-test");
    assert!(output.status.success(), "{}", output_text(&output.stderr));
    let stdout = output_text(&output.stdout);
    for control in [
        "zero-test-discovery",
        "test-command-failure",
        "missing-fixture",
        "required-test-ignored",
    ] {
        assert!(stdout.contains(&format!("NEGATIVE_CONTROL_OK: {control}")));
    }
    assert!(stdout.contains("PHASE_GATE_SELFTEST_OK: P0"));
}

#[test]
fn p0_f08_registry_and_ci_declare_only_p0_capabilities() {
    let root = repository_root();
    let registry: Value = serde_json::from_str(
        &fs::read_to_string(root.join("tests/acceptance/registry.json"))
            .expect("acceptance registry exists"),
    )
    .expect("acceptance registry is JSON");
    let cases = registry["cases"]
        .as_array()
        .expect("registry cases is an array");
    let p0_cases = cases
        .iter()
        .filter(|case| case["phase"] == "P0" && case["required"] == true)
        .collect::<Vec<_>>();
    assert_eq!(p0_cases.len(), 8);
    assert!(
        p0_cases
            .iter()
            .all(|case| case["readiness"] == "implemented")
    );
    let future_cases = cases
        .iter()
        .filter(|case| {
            case["id"]
                .as_str()
                .is_some_and(|id| id.starts_with('C') || id.starts_with('K'))
        })
        .collect::<Vec<_>>();
    assert_eq!(future_cases.len(), 44);
    let p1_cases = future_cases
        .iter()
        .filter(|case| case["phase"] == "P1")
        .collect::<Vec<_>>();
    assert_eq!(p1_cases.len(), 12);
    assert!(
        p1_cases
            .iter()
            .all(|case| { case["readiness"] == "implemented" && case["required"] == true })
    );
    assert!(
        future_cases
            .iter()
            .filter(|case| case["phase"] != "P1")
            .all(|case| { case["readiness"] == "not_implemented" && case["required"] == false })
    );

    let ci =
        fs::read_to_string(root.join(".github/workflows/ci.yml")).expect("P0 CI workflow exists");
    assert!(ci.contains("os: [ubuntu-latest, windows-latest]"));
    assert!(
        ci.contains("rustup toolchain install 1.97.1 --profile minimal --component clippy,rustfmt")
    );
    assert!(ci.contains("scripts/Verify-Phase.ps1 -Phase P0"));
    assert!(!ci.contains("secrets."));
}
