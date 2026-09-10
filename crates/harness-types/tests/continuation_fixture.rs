use std::path::PathBuf;

use harness_types::{ErrorCode, verify_continuation_fixture};

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/p0/continuation")
}

#[test]
fn continuation_fixture_is_complete_and_independent_of_a_runtime_projector() {
    let report = verify_continuation_fixture(&fixture_root()).unwrap();

    assert_eq!(report.fixture_id, "p0-continuation-v1");
    assert_eq!(report.event_count, 10);
    assert_eq!(report.completed_steps, ["plan-inspect", "plan-test-a"]);
    assert_eq!(report.failed_check_count, 1);
    assert_eq!(report.pending_steps, ["plan-fix-b"]);
    assert_eq!(report.current_decision, "B");
    assert_eq!(
        report.next_action,
        "Fix test B, then rerun cargo test parser_b."
    );
    assert_eq!(
        report.negative_controls,
        [
            "duplicate_input_id",
            "unknown_critical_event",
            "truncated_artifact",
            "corrupt_artifact"
        ]
    );
}

#[test]
fn fixture_verifier_fails_closed_when_the_root_is_missing() {
    let missing = fixture_root().join("missing-fixture-root");
    let error = verify_continuation_fixture(&missing).unwrap_err();
    assert_eq!(error.code(), ErrorCode::FixtureIntegrity);
}
