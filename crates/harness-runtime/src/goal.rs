//! Typed goal criteria, proposed verdicts and host acceptance (M3-04).
//!
//! The evaluator never reads prose as proof. It maps typed evidence — committed
//! tool executions, workspace changes, checks and artifacts — onto typed
//! criteria, and the driver decides whether a `needs_work` verdict may be
//! continued. A model that says "done" without the required evidence gets a
//! `needs_work` or `unverified` verdict, never `satisfied`.

use harness_types::{ContentHash, ErrorCode};
use serde::Serialize;
use serde_json::json;

use crate::RuntimeError;

/// What kind of evidence a criterion accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// A non-empty final response.
    Response,
    /// At least one committed tool execution.
    ToolExecution,
    /// At least one committed file change.
    FileChange,
    /// At least one check that ran and passed.
    Check,
    /// At least one published artifact.
    Artifact,
}

impl EvidenceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Response => "response",
            Self::ToolExecution => "tool_execution",
            Self::FileChange => "file_change",
            Self::Check => "check",
            Self::Artifact => "artifact",
        }
    }
}

/// One typed acceptance criterion.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GoalCriterion {
    pub id: String,
    pub description: String,
    pub required: bool,
    pub evidence: EvidenceKind,
}

impl GoalCriterion {
    #[must_use]
    pub fn required(id: impl Into<String>, evidence: EvidenceKind) -> Self {
        let id = id.into();
        Self {
            description: format!("produce {} evidence", evidence.as_str()),
            id,
            required: true,
            evidence,
        }
    }

    #[must_use]
    pub fn optional(id: impl Into<String>, evidence: EvidenceKind) -> Self {
        let id = id.into();
        Self {
            description: format!("produce {} evidence", evidence.as_str()),
            id,
            required: false,
            evidence,
        }
    }
}

/// The goal one run is trying to satisfy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GoalSpec {
    pub objective: String,
    pub criteria: Vec<GoalCriterion>,
    /// How many host continuations one run may spend on this goal.
    pub max_continuations: u32,
    /// How many continuations may repeat without progress before a hard stop.
    pub max_no_progress: u32,
}

impl GoalSpec {
    #[must_use]
    pub fn new(objective: impl Into<String>, criteria: Vec<GoalCriterion>) -> Self {
        Self {
            objective: objective.into(),
            criteria,
            max_continuations: 2,
            max_no_progress: 1,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, max_continuations: u32, max_no_progress: u32) -> Self {
        self.max_continuations = max_continuations;
        self.max_no_progress = max_no_progress;
        self
    }

    #[must_use]
    pub fn required_ids(&self) -> Vec<String> {
        self.criteria
            .iter()
            .filter(|criterion| criterion.required)
            .map(|criterion| criterion.id.clone())
            .collect()
    }
}

/// Everything the host knows about what one run produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GoalEvidence {
    pub response: String,
    pub finish_reason: Option<String>,
    pub truncated: bool,
    pub tool_executions: u32,
    pub successful_tool_executions: u32,
    pub file_changes: u32,
    pub checks_passed: u32,
    pub artifacts: u32,
    pub pending_tool_calls: u32,
}

impl GoalEvidence {
    /// Whether the evidence satisfies one criterion kind.
    #[must_use]
    pub fn satisfies(&self, kind: EvidenceKind) -> bool {
        match kind {
            EvidenceKind::Response => !self.response.trim().is_empty(),
            EvidenceKind::ToolExecution => self.successful_tool_executions > 0,
            EvidenceKind::FileChange => self.file_changes > 0,
            EvidenceKind::Check => self.checks_passed > 0,
            EvidenceKind::Artifact => self.artifacts > 0,
        }
    }

    /// A stable fingerprint of what this evidence proves.
    ///
    /// Two continuations that produce the same fingerprint made no progress,
    /// whatever the model wrote about them.
    #[must_use]
    pub fn progress_signature(&self) -> String {
        let payload = json!({
            "response": self.response.trim(),
            "finish_reason": self.finish_reason,
            "truncated": self.truncated,
            "tool_executions": self.tool_executions,
            "successful_tool_executions": self.successful_tool_executions,
            "file_changes": self.file_changes,
            "checks_passed": self.checks_passed,
            "artifacts": self.artifacts,
            "pending_tool_calls": self.pending_tool_calls,
        });
        ContentHash::from_canonical_json(&payload).map_or_else(
            |_| format!("unhashed:{payload}"),
            |hash| hash.as_str().to_owned(),
        )
    }
}

/// What the evaluator proposes after one terminal model response.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum GoalVerdict {
    /// Every required criterion has evidence.
    Satisfied { satisfied: Vec<String> },
    /// Work remains and a next action exists.
    NeedsWork {
        reason: String,
        next_action: String,
        missing: Vec<String>,
    },
    /// The run cannot continue without a human answer.
    NeedsInput {
        question: String,
        missing: Vec<String>,
    },
    /// The run is waiting on something outside the host.
    ExternalWait {
        detail: String,
        missing: Vec<String>,
    },
    /// The terminal response itself cannot be trusted (empty or capped).
    Unverified {
        reason: String,
        missing: Vec<String>,
    },
}

impl GoalVerdict {
    #[must_use]
    pub const fn acceptance(&self) -> AcceptanceState {
        match self {
            Self::Satisfied { .. } => AcceptanceState::Satisfied,
            Self::NeedsWork { .. } => AcceptanceState::NeedsWork,
            Self::NeedsInput { .. } => AcceptanceState::NeedsInput,
            Self::ExternalWait { .. } => AcceptanceState::ExternalWait,
            Self::Unverified { .. } => AcceptanceState::Unverified,
        }
    }
}

/// Task acceptance, kept separate from the run's stop reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceState {
    /// No goal was attached to the run.
    NotEvaluated,
    Satisfied,
    NeedsWork,
    NeedsInput,
    ExternalWait,
    Unverified,
    Rejected,
}

impl AcceptanceState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotEvaluated => "not_evaluated",
            Self::Satisfied => "satisfied",
            Self::NeedsWork => "needs_work",
            Self::NeedsInput => "needs_input",
            Self::ExternalWait => "external_wait",
            Self::Unverified => "unverified",
            Self::Rejected => "rejected",
        }
    }
}

/// One evaluation, including the fingerprint it was based on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GoalEvaluation {
    pub verdict: GoalVerdict,
    pub progress_signature: String,
}

/// What the evaluator is allowed to see.
pub struct GoalEvaluationInput<'a> {
    pub spec: &'a GoalSpec,
    pub evidence: &'a GoalEvidence,
    pub previous_signature: Option<&'a str>,
    pub continuations: u32,
    pub no_progress: u32,
    pub remaining_budget: Option<u64>,
}

/// The evaluator port. Tests inject scripted evaluators; production uses the
/// host implementation below.
pub trait GoalEvaluator: Send + Sync {
    fn evaluate(&self, input: &GoalEvaluationInput<'_>) -> Result<GoalEvaluation, RuntimeError>;
}

/// The host evaluator: criteria are checked against typed evidence only.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostGoalEvaluator;

impl GoalEvaluator for HostGoalEvaluator {
    fn evaluate(&self, input: &GoalEvaluationInput<'_>) -> Result<GoalEvaluation, RuntimeError> {
        let evidence = input.evidence;
        let signature = evidence.progress_signature();
        if evidence.response.trim().is_empty() {
            return Ok(GoalEvaluation {
                verdict: GoalVerdict::Unverified {
                    reason: "the model returned no final text".to_owned(),
                    missing: input.spec.required_ids(),
                },
                progress_signature: signature,
            });
        }
        if evidence.truncated
            || evidence
                .finish_reason
                .as_deref()
                .is_some_and(|reason| reason == "length" || reason == "max_tokens")
        {
            return Ok(GoalEvaluation {
                verdict: GoalVerdict::Unverified {
                    reason: "the response was cut by the output cap".to_owned(),
                    missing: input.spec.required_ids(),
                },
                progress_signature: signature,
            });
        }
        if evidence.pending_tool_calls > 0 {
            return Ok(GoalEvaluation {
                verdict: GoalVerdict::NeedsWork {
                    reason: "a tool call is still pending".to_owned(),
                    next_action: "settle the pending tool call".to_owned(),
                    missing: input.spec.required_ids(),
                },
                progress_signature: signature,
            });
        }
        let mut satisfied = Vec::new();
        let mut missing = Vec::new();
        for criterion in &input.spec.criteria {
            if evidence.satisfies(criterion.evidence) {
                satisfied.push(criterion.id.clone());
            } else if criterion.required {
                missing.push(criterion.id.clone());
            }
        }
        if missing.is_empty() {
            return Ok(GoalEvaluation {
                verdict: GoalVerdict::Satisfied { satisfied },
                progress_signature: signature,
            });
        }
        let kinds = input
            .spec
            .criteria
            .iter()
            .filter(|criterion| missing.contains(&criterion.id))
            .map(|criterion| criterion.evidence.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        Ok(GoalEvaluation {
            verdict: GoalVerdict::NeedsWork {
                reason: "required criteria still lack evidence".to_owned(),
                next_action: format!("produce {kinds} evidence for {}", missing.join(", ")),
                missing,
            },
            progress_signature: signature,
        })
    }
}

/// The evaluator used when a goal is attached but no custom one was injected.
#[must_use]
pub fn default_evaluator() -> std::sync::Arc<dyn GoalEvaluator> {
    std::sync::Arc::new(HostGoalEvaluator)
}

/// Reject an evaluator result that is not a verdict the driver can act on.
pub fn validate_evaluation(evaluation: &GoalEvaluation) -> Result<(), RuntimeError> {
    match &evaluation.verdict {
        GoalVerdict::NeedsWork { next_action, .. } if next_action.trim().is_empty() => {
            Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a needs_work verdict must name a next action",
            ))
        }
        GoalVerdict::NeedsInput { question, .. } if question.trim().is_empty() => {
            Err(RuntimeError::new(
                ErrorCode::InvalidPayload,
                "a needs_input verdict must carry a question",
            ))
        }
        _ => Ok(()),
    }
}
