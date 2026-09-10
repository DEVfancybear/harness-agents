//! Static P0 continuation-fixture verification.
//!
//! This module deliberately validates source evidence and the independently
//! authored expected state. It does not project events into runtime state and
//! therefore makes no P1 recovery claim.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;

use crate::{ContentHash, ErrorCode, EventEnvelope, HarnessError, P0_SCHEMA_VERSION};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinuationFixtureReport {
    pub fixture_id: String,
    pub event_count: usize,
    pub completed_steps: Vec<String>,
    pub failed_check_count: usize,
    pub pending_steps: Vec<String>,
    pub current_decision: String,
    pub next_action: String,
    pub negative_controls: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureManifest {
    schema_version: u16,
    fixture_id: String,
    main_events: String,
    expected_state: String,
    expected_event_count: usize,
    negative_cases: Vec<NegativeCase>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NegativeCase {
    id: String,
    path: String,
    expected_error: Option<String>,
    expected_outcome: Option<String>,
    expected_hash: Option<String>,
    must_mismatch: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedState {
    schema_version: u16,
    fixture_id: String,
    objective: String,
    requirements: Vec<ExpectedRequirement>,
    completed_steps: Vec<String>,
    failed_checks: Vec<ExpectedCheck>,
    pending_steps: Vec<String>,
    current_decision: ExpectedDecision,
    unclassified_input_id: String,
    next_action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedRequirement {
    input_id: String,
    text: String,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedCheck {
    command: String,
    outcome: String,
    tested_revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedDecision {
    value: String,
    supersedes_value: String,
}

/// Verify P0's continuation source fixture without constructing a runtime
/// projection. Every check is deterministic and requires no provider key.
pub fn verify_continuation_fixture(root: &Path) -> Result<ContinuationFixtureReport, HarnessError> {
    let manifest: FixtureManifest = read_json(root, "fixture-manifest.json")?;
    if manifest.schema_version != P0_SCHEMA_VERSION {
        return Err(fixture_error(
            "fixture manifest has an unsupported schema version",
        ));
    }
    let events = read_events(root, &manifest.main_events)?;
    verify_main_events(&events, manifest.expected_event_count)?;
    let expected: ExpectedState = read_json(root, &manifest.expected_state)?;
    verify_expected_state(&manifest.fixture_id, &expected, &events)?;
    let negative_controls = verify_negative_cases(root, &manifest.negative_cases)?;

    Ok(ContinuationFixtureReport {
        fixture_id: manifest.fixture_id,
        event_count: events.len(),
        completed_steps: expected.completed_steps,
        failed_check_count: expected.failed_checks.len(),
        pending_steps: expected.pending_steps,
        current_decision: expected.current_decision.value,
        next_action: expected.next_action,
        negative_controls,
    })
}

fn verify_main_events(events: &[EventEnvelope], expected_count: usize) -> Result<(), HarnessError> {
    if events.len() != expected_count {
        return Err(fixture_error("main event count does not match manifest"));
    }
    let Some(first) = events.first() else {
        return Err(fixture_error(
            "main fixture must contain at least one event",
        ));
    };
    let mut event_ids = BTreeSet::new();
    for (index, event) in events.iter().enumerate() {
        event.validate().map_err(|error| {
            fixture_error(&format!("main event {} is invalid: {error}", index + 1))
        })?;
        let expected_sequence =
            u64::try_from(index + 1).map_err(|_| fixture_error("fixture sequence exceeds u64"))?;
        if event.seq != expected_sequence {
            return Err(fixture_error(
                "main event sequences must be contiguous from 1",
            ));
        }
        if event.session_id != first.session_id {
            return Err(fixture_error("main event fixture mixes session IDs"));
        }
        if !event_ids.insert(event.event_id.as_str()) {
            return Err(fixture_error(
                "main event fixture contains a duplicate event ID",
            ));
        }
    }
    Ok(())
}

fn verify_expected_state(
    manifest_id: &str,
    expected: &ExpectedState,
    events: &[EventEnvelope],
) -> Result<(), HarnessError> {
    if expected.schema_version != P0_SCHEMA_VERSION || expected.fixture_id != manifest_id {
        return Err(fixture_error(
            "expected state identity or schema version does not match manifest",
        ));
    }
    if expected.objective.trim().is_empty() || expected.next_action.trim().is_empty() {
        return Err(fixture_error(
            "expected state must include objective and next action",
        ));
    }
    if expected.requirements.len() != 3
        || expected.completed_steps.len() != 2
        || expected.failed_checks.len() != 1
        || expected.pending_steps.len() != 1
    {
        return Err(fixture_error(
            "expected state must contain three requirements, two completed steps, one failed check, and one pending step",
        ));
    }
    let mut input_ids = BTreeSet::new();
    for requirement in &expected.requirements {
        if requirement.status != "admitted"
            || requirement.text.trim().is_empty()
            || !input_ids.insert(requirement.input_id.as_str())
        {
            return Err(fixture_error("expected requirements are invalid"));
        }
        if !event_input_ids(events).contains(requirement.input_id.as_str()) {
            return Err(fixture_error("expected requirement has no source event"));
        }
    }
    if !event_input_ids(events).contains(expected.unclassified_input_id.as_str()) {
        return Err(fixture_error("unclassified input has no source event"));
    }
    let unclassified = events.iter().any(|event| {
        event
            .payload
            .get("input_id")
            .and_then(Value::as_str)
            .is_some_and(|id| id == expected.unclassified_input_id)
            && event
                .payload
                .get("classification")
                .and_then(Value::as_str)
                .is_some_and(|classification| classification == "unclassified")
    });
    if !unclassified {
        return Err(fixture_error(
            "unclassified input is not represented as source evidence",
        ));
    }
    for step in &expected.completed_steps {
        if !has_plan_event(events, step, "completed") {
            return Err(fixture_error(
                "completed step has no completed source event",
            ));
        }
    }
    for step in &expected.pending_steps {
        if !has_plan_event(events, step, "pending") {
            return Err(fixture_error("pending step has no pending source event"));
        }
    }
    for check in &expected.failed_checks {
        if check.outcome != "failed" || !has_check_event(events, check) {
            return Err(fixture_error("failed check has no matching source event"));
        }
    }
    let decision_a = has_decision(events, &expected.current_decision.supersedes_value, None);
    let decision_b = has_decision(
        events,
        &expected.current_decision.value,
        Some("supersedes_event_id"),
    );
    if !decision_a || !decision_b {
        return Err(fixture_error(
            "decision supersession is not represented in source events",
        ));
    }
    Ok(())
}

fn event_input_ids(events: &[EventEnvelope]) -> BTreeSet<&str> {
    events
        .iter()
        .filter_map(|event| event.payload.get("input_id").and_then(Value::as_str))
        .collect()
}

fn has_plan_event(events: &[EventEnvelope], plan_item_id: &str, status: &str) -> bool {
    events.iter().any(|event| {
        event.event_type == "plan.step_recorded"
            && event
                .payload
                .get("plan_item_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == plan_item_id)
            && event
                .payload
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|value| value == status)
    })
}

fn has_check_event(events: &[EventEnvelope], check: &ExpectedCheck) -> bool {
    events.iter().any(|event| {
        event.event_type == "check.recorded"
            && event
                .payload
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|value| value == check.command)
            && event
                .payload
                .get("outcome")
                .and_then(Value::as_str)
                .is_some_and(|value| value == check.outcome)
            && event
                .payload
                .get("tested_revision")
                .and_then(Value::as_str)
                .is_some_and(|value| value == check.tested_revision)
    })
}

fn has_decision(events: &[EventEnvelope], value: &str, required_key: Option<&str>) -> bool {
    events.iter().any(|event| {
        event.event_type == "decision.recorded"
            && event
                .payload
                .get("value")
                .and_then(Value::as_str)
                .is_some_and(|candidate| candidate == value)
            && required_key.is_none_or(|key| event.payload.contains_key(key))
    })
}

fn verify_negative_cases(root: &Path, cases: &[NegativeCase]) -> Result<Vec<String>, HarnessError> {
    let expected_ids = BTreeSet::from([
        "duplicate_input_id",
        "unknown_critical_event",
        "truncated_artifact",
        "corrupt_artifact",
    ]);
    let actual_ids = cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    if actual_ids != expected_ids {
        return Err(fixture_error(
            "negative fixture cases do not match the P0 contract",
        ));
    }

    let mut verified = Vec::new();
    for case in cases {
        match case.id.as_str() {
            "duplicate_input_id" => {
                if case.expected_error.as_deref() != Some("duplicate_input_id") {
                    return Err(fixture_error("duplicate input case lacks expected error"));
                }
                let events = read_events(root, &case.path)?;
                if !has_duplicate_input_id(&events)? {
                    return Err(fixture_error(
                        "duplicate input negative control did not duplicate input_id",
                    ));
                }
            }
            "unknown_critical_event" => {
                if case.expected_outcome.as_deref() != Some("blocked") {
                    return Err(fixture_error(
                        "unknown critical event case lacks blocked outcome",
                    ));
                }
                let events = read_events(root, &case.path)?;
                if events.len() != 1
                    || !events[0].continuity_critical
                    || is_known_event_type(&events[0].event_type)
                {
                    return Err(fixture_error("unknown critical event control is not valid"));
                }
            }
            "truncated_artifact" | "corrupt_artifact" => {
                if case.must_mismatch != Some(true) {
                    return Err(fixture_error(
                        "artifact corruption case must require a mismatch",
                    ));
                }
                let expected_hash =
                    ContentHash::parse(case.expected_hash.clone().ok_or_else(|| {
                        fixture_error("artifact corruption case lacks expected hash")
                    })?)?;
                let actual_hash = ContentHash::from_bytes(&read_file(root, &case.path)?);
                if actual_hash == expected_hash {
                    return Err(fixture_error(
                        "artifact corruption control unexpectedly matches",
                    ));
                }
            }
            _ => return Err(fixture_error("unsupported negative fixture case")),
        }
        verified.push(case.id.clone());
    }
    Ok(verified)
}

fn has_duplicate_input_id(events: &[EventEnvelope]) -> Result<bool, HarnessError> {
    let mut input_ids = BTreeSet::new();
    for event in events {
        event.validate()?;
        if event.event_type == "input.admitted" {
            let input_id = event
                .payload
                .get("input_id")
                .and_then(Value::as_str)
                .ok_or_else(|| fixture_error("input event has no input_id"))?;
            if !input_ids.insert(input_id) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn is_known_event_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "input.admitted" | "plan.step_recorded" | "decision.recorded" | "check.recorded"
    )
}

fn read_events(root: &Path, path: &str) -> Result<Vec<EventEnvelope>, HarnessError> {
    let contents = String::from_utf8(read_file(root, path)?)
        .map_err(|_| fixture_error("fixture event file must be valid UTF-8"))?;
    if contents.is_empty() {
        return Err(fixture_error("fixture event file must not be empty"));
    }
    contents
        .lines()
        .enumerate()
        .map(|(index, line)| {
            if line.trim().is_empty() {
                return Err(fixture_error("fixture event file contains an empty line"));
            }
            EventEnvelope::parse_json(line).map_err(|error| {
                fixture_error(&format!(
                    "fixture event line {} is invalid: {error}",
                    index + 1
                ))
            })
        })
        .collect()
}

fn read_json<T>(root: &Path, path: &str) -> Result<T, HarnessError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(&read_file(root, path)?)
        .map_err(|_| fixture_error("fixture JSON file is invalid"))
}

fn read_file(root: &Path, relative_path: &str) -> Result<Vec<u8>, HarnessError> {
    let path = safe_fixture_path(root, relative_path)?;
    fs::read(path).map_err(|_| fixture_error("required fixture file is missing or unreadable"))
}

fn safe_fixture_path(root: &Path, relative_path: &str) -> Result<PathBuf, HarnessError> {
    let candidate = Path::new(relative_path);
    if candidate.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(fixture_error(
            "fixture path must remain under its fixture root",
        ));
    }
    Ok(root.join(candidate))
}

fn fixture_error(message: &str) -> HarnessError {
    HarnessError::new(ErrorCode::FixtureIntegrity, message)
}
