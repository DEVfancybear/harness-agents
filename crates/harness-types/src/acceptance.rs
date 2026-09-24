//! Task acceptance is a decision of its own, not an alias for "the run ended".
//!
//! A completed run is a terminal *run* state. A task is accepted only when
//! every required criterion is satisfied with typed evidence, no pending effect
//! or check remains, and the decision is recorded. A human acceptance is
//! recorded with its actor and source; it never fabricates test evidence and
//! never marks a criterion satisfied.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AgentRunId, ArtifactId, CheckOutcome, ContentHash, ErrorCode, HarnessError, P0_SCHEMA_VERSION,
    SourceRef, TaskId,
};

/// Whether one criterion is satisfied.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CriterionStatus {
    Pending,
    Satisfied,
    Failed,
}

/// Typed evidence for one criterion (CONTRACTS §6).
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum CriterionEvidence {
    /// Typed evidence already verified by the host goal evaluator and committed
    /// with a run. The source names the run response or a settled tool receipt.
    RunObserved {
        run_id: AgentRunId,
        evidence_kind: String,
        source: String,
        content_hash: ContentHash,
    },
    FileChanged {
        path: String,
        before: Option<ContentHash>,
        after: Option<ContentHash>,
        receipt_ref: Option<String>,
    },
    CheckExecuted {
        command: String,
        workspace_digest: Option<ContentHash>,
        exit_code: Option<i32>,
        outcome: CheckOutcome,
        receipt_ref: Option<String>,
    },
    ArtifactProduced {
        artifact_id: Option<ArtifactId>,
        reference: String,
    },
}

impl CriterionEvidence {
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::RunObserved { .. } => "run_observed",
            Self::FileChanged { .. } => "file_changed",
            Self::CheckExecuted { .. } => "check_executed",
            Self::ArtifactProduced { .. } => "artifact_produced",
        }
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        let (reference, label) = match self {
            Self::RunObserved {
                evidence_kind,
                source,
                ..
            } => {
                if !matches!(
                    evidence_kind.as_str(),
                    "response" | "tool_execution" | "file_change" | "check" | "artifact"
                ) {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "run_observed.kind is invalid",
                    ));
                }
                (source.as_str(), "run_observed.source")
            }
            Self::FileChanged { path, .. } => (path.as_str(), "file_changed.path"),
            Self::CheckExecuted { command, .. } => (command.as_str(), "check_executed.command"),
            Self::ArtifactProduced { reference, .. } => {
                (reference.as_str(), "artifact_produced.reference")
            }
        };
        if reference.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("{label} must not be empty"),
            ));
        }
        Ok(())
    }
}

/// One criterion of the task's acceptance contract.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionState {
    pub criterion_id: String,
    pub required: bool,
    pub status: CriterionStatus,
    pub evidence: Vec<CriterionEvidence>,
}

impl CriterionState {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.criterion_id.trim().is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "criterion_id must not be empty",
            ));
        }
        if matches!(self.status, CriterionStatus::Satisfied) && self.evidence.is_empty() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!(
                    "criterion {} is satisfied without evidence",
                    self.criterion_id
                ),
            ));
        }
        for evidence in &self.evidence {
            evidence.validate()?;
        }
        Ok(())
    }
}

/// Who made the acceptance decision.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum AcceptanceActor {
    Automatic,
    Human { actor_id: String },
}

impl AcceptanceActor {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Human { .. } => "human",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceDecision {
    NotAccepted,
    Accepted,
}

/// The durable acceptance state of one task.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceRecord {
    pub schema_version: u16,
    pub task_id: TaskId,
    pub criteria: Vec<CriterionState>,
    /// Checks or effects that are still unproven. Any pending effect keeps the
    /// task unaccepted.
    pub pending_effects: u64,
    pub evidence_fingerprint: Option<ContentHash>,
    pub decision: AcceptanceDecision,
    pub decided_by: AcceptanceActor,
    pub decision_source: Option<SourceRef>,
}

/// A validated domain command for the acceptance machine.
#[derive(Clone, Debug)]
pub enum AcceptanceCommand {
    Evaluate {
        criteria: Vec<CriterionState>,
        pending_effects: u64,
        evidence_fingerprint: Option<ContentHash>,
    },
    HumanAccept {
        actor_id: String,
        source: SourceRef,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AcceptanceEvent {
    Evaluated {
        satisfied: usize,
        required: usize,
        accepted: bool,
    },
    Accepted {
        actor: AcceptanceActor,
    },
    HumanOverride {
        actor_id: String,
    },
}

/// The reducer result: next state plus the events that explain it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptanceTransition {
    pub next: AcceptanceRecord,
    pub events: Vec<AcceptanceEvent>,
}

impl AcceptanceRecord {
    #[must_use]
    pub fn initial(task_id: TaskId) -> Self {
        Self {
            schema_version: P0_SCHEMA_VERSION,
            task_id,
            criteria: Vec::new(),
            pending_effects: 0,
            evidence_fingerprint: None,
            decision: AcceptanceDecision::NotAccepted,
            decided_by: AcceptanceActor::Automatic,
            decision_source: None,
        }
    }

    #[must_use]
    pub const fn is_accepted(&self) -> bool {
        matches!(self.decision, AcceptanceDecision::Accepted)
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.schema_version != P0_SCHEMA_VERSION {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "acceptance record schema {} is not supported",
                    self.schema_version
                ),
            ));
        }
        if let AcceptanceActor::Human { actor_id } = &self.decided_by
            && actor_id.trim().is_empty()
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "a human acceptance requires an actor id",
            ));
        }
        if matches!(self.decided_by, AcceptanceActor::Human { .. })
            && self.decision_source.is_none()
        {
            return Err(HarnessError::new(
                ErrorCode::MissingAuthority,
                "a human acceptance requires the source it was recorded from",
            ));
        }
        validate_criteria(&self.criteria)?;
        if matches!(self.decided_by, AcceptanceActor::Automatic) && self.decision_source.is_some() {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                "an automatic acceptance cannot name a human decision source",
            ));
        }
        if matches!(self.decision, AcceptanceDecision::Accepted) && self.pending_effects != 0 {
            return Err(HarnessError::new(
                ErrorCode::InvalidStateTransition,
                "acceptance is blocked while effects are pending",
            ));
        }
        if matches!(self.decision, AcceptanceDecision::Accepted)
            && matches!(self.decided_by, AcceptanceActor::Automatic)
        {
            let required = self
                .criteria
                .iter()
                .filter(|item| item.required)
                .collect::<Vec<_>>();
            if required.is_empty()
                || self.pending_effects != 0
                || required
                    .iter()
                    .any(|item| !matches!(item.status, CriterionStatus::Satisfied))
            {
                return Err(HarnessError::new(
                    ErrorCode::InvalidStateTransition,
                    "automatic acceptance requires satisfied required criteria and no pending effects",
                ));
            }
            for criterion in required {
                for evidence in &criterion.evidence {
                    if let CriterionEvidence::CheckExecuted {
                        workspace_digest,
                        exit_code,
                        outcome,
                        ..
                    } = evidence
                        && (*outcome != CheckOutcome::Passed
                            || *exit_code != Some(0)
                            || workspace_digest.is_none()
                            || workspace_digest.as_ref() != self.evidence_fingerprint.as_ref())
                    {
                        return Err(HarnessError::new(
                            ErrorCode::InvalidPayload,
                            format!(
                                "accepted check evidence for {} must pass with exit code 0 at the accepted workspace fingerprint",
                                criterion.criterion_id
                            ),
                        ));
                    }
                }
            }
        }
        if matches!(self.decided_by, AcceptanceActor::Human { .. })
            && !matches!(self.decision, AcceptanceDecision::Accepted)
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidStateTransition,
                "a human decision source is only valid for an accepted override",
            ));
        }
        Ok(())
    }

    /// Apply one validated command. Acceptance is terminal: once accepted, no
    /// later evaluation or override changes the decision.
    pub fn apply(&self, command: AcceptanceCommand) -> Result<AcceptanceTransition, HarnessError> {
        self.validate()?;
        if self.is_accepted() {
            return Err(HarnessError::new(
                ErrorCode::InvalidStateTransition,
                "task acceptance is terminal",
            ));
        }
        match command {
            AcceptanceCommand::Evaluate {
                criteria,
                pending_effects,
                evidence_fingerprint,
            } => {
                validate_criteria(&criteria)?;
                let required = criteria
                    .iter()
                    .filter(|criterion| criterion.required)
                    .count();
                if required == 0 {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "automatic acceptance requires at least one required criterion",
                    ));
                }
                let satisfied = criteria
                    .iter()
                    .filter(|criterion| {
                        criterion.required && matches!(criterion.status, CriterionStatus::Satisfied)
                    })
                    .count();
                let accepted = required == satisfied && pending_effects == 0;
                let mut events = vec![AcceptanceEvent::Evaluated {
                    satisfied,
                    required,
                    accepted,
                }];
                if accepted {
                    events.push(AcceptanceEvent::Accepted {
                        actor: AcceptanceActor::Automatic,
                    });
                }
                let next = Self {
                    schema_version: P0_SCHEMA_VERSION,
                    task_id: self.task_id.clone(),
                    criteria,
                    pending_effects,
                    evidence_fingerprint,
                    decision: if accepted {
                        AcceptanceDecision::Accepted
                    } else {
                        AcceptanceDecision::NotAccepted
                    },
                    decided_by: AcceptanceActor::Automatic,
                    decision_source: None,
                };
                next.validate()?;
                Ok(AcceptanceTransition { next, events })
            }
            AcceptanceCommand::HumanAccept { actor_id, source } => {
                if actor_id.trim().is_empty() {
                    return Err(HarnessError::new(
                        ErrorCode::InvalidPayload,
                        "a human acceptance requires an actor id",
                    ));
                }
                source.validate()?;
                let next = Self {
                    schema_version: P0_SCHEMA_VERSION,
                    task_id: self.task_id.clone(),
                    // Criteria are deliberately untouched: an override is a
                    // recorded human decision, not fabricated test evidence.
                    criteria: self.criteria.clone(),
                    pending_effects: self.pending_effects,
                    evidence_fingerprint: self.evidence_fingerprint.clone(),
                    decision: AcceptanceDecision::Accepted,
                    decided_by: AcceptanceActor::Human {
                        actor_id: actor_id.clone(),
                    },
                    decision_source: Some(source),
                };
                next.validate()?;
                Ok(AcceptanceTransition {
                    next,
                    events: vec![
                        AcceptanceEvent::HumanOverride {
                            actor_id: actor_id.clone(),
                        },
                        AcceptanceEvent::Accepted {
                            actor: AcceptanceActor::Human { actor_id },
                        },
                    ],
                })
            }
        }
    }
}

fn validate_criteria(criteria: &[CriterionState]) -> Result<(), HarnessError> {
    let mut seen = std::collections::BTreeSet::new();
    for criterion in criteria {
        criterion.validate()?;
        if !seen.insert(criterion.criterion_id.as_str()) {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("duplicate criterion {}", criterion.criterion_id),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AcceptanceActor, AcceptanceCommand, AcceptanceDecision, AcceptanceEvent, AcceptanceRecord,
        CriterionEvidence, CriterionState, CriterionStatus,
    };
    use crate::{
        CheckOutcome, ContentHash, ErrorCode, EventId, P0_SCHEMA_VERSION, SourceRef, TaskId,
    };

    fn source() -> SourceRef {
        SourceRef {
            event_id: EventId::generate(),
            sequence: 1,
            content_hash: ContentHash::from_bytes(b"decision"),
        }
    }

    fn criterion(id: &str, required: bool, status: CriterionStatus) -> CriterionState {
        CriterionState {
            criterion_id: id.to_owned(),
            required,
            status,
            evidence: if matches!(status, CriterionStatus::Satisfied) {
                vec![CriterionEvidence::CheckExecuted {
                    command: "cargo test -p harness-types".to_owned(),
                    workspace_digest: Some(ContentHash::from_bytes(b"evidence")),
                    exit_code: Some(0),
                    outcome: CheckOutcome::Passed,
                    receipt_ref: Some("tool_execution_fixture".to_owned()),
                }]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn a_completed_run_with_a_missing_criterion_is_not_an_accepted_task() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let transition = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![
                    criterion("tests-pass", true, CriterionStatus::Satisfied),
                    criterion("final-review", true, CriterionStatus::Pending),
                ],
                pending_effects: 0,
                evidence_fingerprint: Some(ContentHash::from_bytes(b"evidence")),
            })
            .expect("evaluation is allowed");
        assert_eq!(transition.next.decision, AcceptanceDecision::NotAccepted);
        assert!(!transition.next.is_accepted());
        assert_eq!(
            transition.events,
            vec![AcceptanceEvent::Evaluated {
                satisfied: 1,
                required: 2,
                accepted: false,
            }]
        );
    }

    #[test]
    fn pending_effects_keep_a_satisfied_task_unaccepted() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let transition = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![criterion("tests-pass", true, CriterionStatus::Satisfied)],
                pending_effects: 1,
                evidence_fingerprint: None,
            })
            .expect("evaluation is allowed");
        assert_eq!(transition.next.decision, AcceptanceDecision::NotAccepted);
        assert_eq!(
            transition
                .next
                .apply(AcceptanceCommand::HumanAccept {
                    actor_id: "operator".to_owned(),
                    source: source(),
                })
                .expect_err("human override cannot accept unresolved effects")
                .code(),
            ErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn satisfied_criteria_with_evidence_are_accepted_and_acceptance_is_terminal() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let transition = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![
                    criterion("tests-pass", true, CriterionStatus::Satisfied),
                    criterion("optional-note", false, CriterionStatus::Pending),
                ],
                pending_effects: 0,
                evidence_fingerprint: Some(ContentHash::from_bytes(b"evidence")),
            })
            .expect("evaluation is allowed");
        assert!(transition.next.is_accepted());
        assert_eq!(
            transition.next.decided_by,
            AcceptanceActor::Automatic,
            "an evaluation never claims a human decision"
        );
        assert!(transition.events.contains(&AcceptanceEvent::Accepted {
            actor: AcceptanceActor::Automatic
        }));

        // Every transition out of an accepted task fails, including another accept.
        for command in [
            AcceptanceCommand::Evaluate {
                criteria: vec![criterion("tests-pass", true, CriterionStatus::Failed)],
                pending_effects: 0,
                evidence_fingerprint: None,
            },
            AcceptanceCommand::HumanAccept {
                actor_id: "operator".to_owned(),
                source: source(),
            },
        ] {
            assert_eq!(
                transition
                    .next
                    .apply(command)
                    .expect_err("acceptance is terminal")
                    .code(),
                ErrorCode::InvalidStateTransition
            );
        }
    }

    #[test]
    fn human_acceptance_records_actor_and_source_without_faking_evidence() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let transition = record
            .apply(AcceptanceCommand::HumanAccept {
                actor_id: "duong".to_owned(),
                source: source(),
            })
            .expect("a human acceptance is allowed while unaccepted");
        assert!(transition.next.is_accepted());
        assert_eq!(
            transition.next.decided_by,
            AcceptanceActor::Human {
                actor_id: "duong".to_owned()
            }
        );
        assert!(transition.next.decision_source.is_some());
        assert_eq!(
            transition.next.criteria,
            Vec::new(),
            "an override must not invent satisfied criteria"
        );
        assert!(matches!(
            transition.events.first(),
            Some(AcceptanceEvent::HumanOverride { .. })
        ));
        assert_eq!(
            record
                .apply(AcceptanceCommand::HumanAccept {
                    actor_id: " ".to_owned(),
                    source: source(),
                })
                .expect_err("an unnamed actor is invalid")
                .code(),
            ErrorCode::InvalidPayload
        );
    }

    #[test]
    fn satisfied_without_evidence_is_rejected_before_it_can_be_recorded() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let error = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![CriterionState {
                    criterion_id: "tests-pass".to_owned(),
                    required: true,
                    status: CriterionStatus::Satisfied,
                    evidence: Vec::new(),
                }],
                pending_effects: 0,
                evidence_fingerprint: None,
            })
            .expect_err("satisfied without evidence is not recordable");
        assert_eq!(error.code(), ErrorCode::InvalidPayload);
    }

    #[test]
    fn automatic_acceptance_cannot_be_vacuous_or_forged() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let empty = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: Vec::new(),
                pending_effects: 0,
                evidence_fingerprint: None,
            })
            .expect_err("empty criteria must not satisfy by vacuous truth");
        assert_eq!(empty.code(), ErrorCode::InvalidPayload);

        let forged = AcceptanceRecord {
            schema_version: P0_SCHEMA_VERSION,
            task_id: TaskId::generate(),
            criteria: vec![criterion("tests-pass", true, CriterionStatus::Pending)],
            pending_effects: 0,
            evidence_fingerprint: None,
            decision: AcceptanceDecision::Accepted,
            decided_by: AcceptanceActor::Automatic,
            decision_source: None,
        };
        assert_eq!(
            forged
                .validate()
                .expect_err("stored acceptance is inconsistent")
                .code(),
            ErrorCode::InvalidStateTransition
        );
    }

    #[test]
    fn accepted_check_evidence_must_match_the_final_workspace_fingerprint() {
        let record = AcceptanceRecord::initial(TaskId::generate());
        let error = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![CriterionState {
                    criterion_id: "tests-pass".to_owned(),
                    required: true,
                    status: CriterionStatus::Satisfied,
                    evidence: vec![CriterionEvidence::CheckExecuted {
                        command: "cargo test".to_owned(),
                        workspace_digest: Some(ContentHash::from_bytes(b"old-workspace")),
                        exit_code: Some(0),
                        outcome: CheckOutcome::Passed,
                        receipt_ref: Some("receipt:check-1".to_owned()),
                    }],
                }],
                pending_effects: 0,
                evidence_fingerprint: Some(ContentHash::from_bytes(b"new-workspace")),
            })
            .expect_err("a check before the final edit cannot accept the task");
        assert_eq!(error.code(), ErrorCode::InvalidPayload);

        let error = record
            .apply(AcceptanceCommand::Evaluate {
                criteria: vec![CriterionState {
                    criterion_id: "tests-pass".to_owned(),
                    required: true,
                    status: CriterionStatus::Satisfied,
                    evidence: vec![CriterionEvidence::CheckExecuted {
                        command: "cargo test".to_owned(),
                        workspace_digest: None,
                        exit_code: Some(0),
                        outcome: CheckOutcome::Passed,
                        receipt_ref: Some("receipt:check-1".to_owned()),
                    }],
                }],
                pending_effects: 0,
                evidence_fingerprint: None,
            })
            .expect_err("a check without a workspace digest cannot accept the task");
        assert_eq!(error.code(), ErrorCode::InvalidPayload);
    }
}
