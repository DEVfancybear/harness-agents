//! A task's strategy, the evidence on it, and the decision to keep it or not.
//!
//! "What Is Missing from AI Post-Training AI" (arXiv 2608.19072) found agents
//! that execute well but lock into the strategy they start with: 2.1% of
//! adjacent attempts changed it, and the agent took 22 of 22 suggestions that
//! kept its strategy and none of the 21 that changed it. A journal of evidence
//! did not move it, more reasoning did not, and "reconsider" with no
//! alternative named did not; one mid-run instruction that named another
//! strategy gained up to 17 points. The paper's remedy is a protocol: after
//! every evaluation, an explicit decision to continue or switch, made before
//! anything else, with named alternatives when the strategy stops paying.
//!
//! Here a task names its strategy and keeps a journal (the paper's typed
//! experiment journal). The host records every evaluation it runs - a goal
//! asked to complete and refused, the project's checks, the verifier - as a
//! score, and after each one the task's state says a decision is due. The
//! decision advises, it does not block (the maintainer's choice): the model
//! weighs the evidence and records continue or switch. When the score has not
//! improved for [`PLATEAU`] evaluations under one strategy, a strategist with a
//! fresh context names alternatives, and the decision carries them.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

/// Evaluations under one strategy, the best of them first, after which it is
/// said to have stopped paying.
pub const PLATEAU: usize = 3;

/// How many journal entries a task keeps.
pub const MAX_JOURNAL: usize = 40;

/// How many evaluations a task keeps.
pub const MAX_EVALUATIONS: usize = 30;

/// How many strategy decisions a task keeps.
pub const MAX_DECISIONS: usize = 30;

/// How many journal entries every turn is shown.
const SHOWN_JOURNAL: usize = 6;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Strategy {
    pub name: String,
    #[serde(default)]
    pub why: String,
    /// The strategies weighed against it when it was chosen.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Alternative {
    pub name: String,
    #[serde(default)]
    pub why: String,
}

/// The paper's journal entry kinds.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    #[default]
    Observation,
    Plan,
    Lesson,
    Eval,
    EvalAnalysis,
}

impl JournalKind {
    const fn label(self) -> &'static str {
        match self {
            Self::Observation => "observation",
            Self::Plan => "plan",
            Self::Lesson => "lesson",
            Self::Eval => "eval",
            Self::EvalAnalysis => "eval analysis",
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalEntry {
    pub kind: JournalKind,
    pub text: String,
    /// The strategy the task followed when it was written.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy: String,
}

/// One evaluation the host ran, as a score: how far the work got
/// (`stage`, higher is further) and how much was left at that stage
/// (`left`, lower is better).
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Evaluation {
    /// Counts every evaluation of the task, kept or not.
    pub seq: usize,
    /// `checklist`, `unchanged`, `checks`, `verifier` or `passed`.
    pub source: String,
    pub stage: u8,
    pub left: u32,
    /// How much the failure left behind at its stage - the lines of the
    /// failing check's output, of the verifier's findings - lower is better.
    /// With one check, `left` is always 1; a test run going from forty
    /// failures to two is progress the output's length shows.
    #[serde(default)]
    pub residue: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub strategy: String,
}

impl Evaluation {
    /// Whether `self` got further than `other`.
    fn better_than(&self, other: &Self) -> bool {
        (
            self.stage,
            std::cmp::Reverse(self.left),
            std::cmp::Reverse(self.residue),
        ) > (
            other.stage,
            std::cmp::Reverse(other.left),
            std::cmp::Reverse(other.residue),
        )
    }
}

/// The stages an evaluation reaches, in order.
pub mod stage {
    /// The checklist still had open items.
    pub const CHECKLIST: u8 = 0;
    /// The project's checks failed.
    pub const CHECKS: u8 = 1;
    /// The checks passed and the verifier did not accept the work.
    pub const VERIFIER: u8 = 2;
    /// Everything passed.
    pub const PASSED: u8 = 3;
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    #[default]
    Continue,
    Switch,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StrategyDecision {
    pub kind: DecisionKind,
    #[serde(default)]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub to: String,
    pub evidence: String,
    /// The last evaluation's `seq` when it was made.
    pub after: usize,
    /// It was made at a plateau.
    #[serde(default)]
    pub plateau: bool,
    /// A switch to a strategy the strategist named.
    #[serde(default)]
    pub proposed: bool,
}

/// Everything about a task's strategy, kept in its task state.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct StrategyState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Strategy>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal: Vec<JournalEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluations: Vec<Evaluation>,
    /// Every evaluation ever recorded, so a decision can say which it followed.
    #[serde(default)]
    pub evaluated: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<StrategyDecision>,
    /// Alternatives the strategist named at the last plateau, until a decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<Alternative>,
    /// The evaluation after which the proposals were made.
    #[serde(default)]
    pub proposed_after: usize,
}

/// Where the task stands after an evaluation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Standing {
    pub plateau: bool,
    /// The strategist should be asked for alternatives: a plateau, and none
    /// named since the last decision.
    pub wants_proposals: bool,
}

impl StrategyState {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.current.is_none()
            && self.journal.is_empty()
            && self.evaluations.is_empty()
            && self.decisions.is_empty()
    }

    fn strategy_name(&self) -> String {
        self.current
            .as_ref()
            .map(|strategy| strategy.name.clone())
            .unwrap_or_default()
    }

    /// Record an evaluation the host ran and say where the task stands.
    pub fn evaluate(
        &mut self,
        source: &str,
        stage: u8,
        left: u32,
        residue: u32,
        detail: &str,
    ) -> Standing {
        self.evaluated += 1;
        let detail = clipped(detail, 300);
        self.evaluations.push(Evaluation {
            seq: self.evaluated,
            source: source.to_owned(),
            stage,
            left,
            residue,
            detail,
            strategy: self.strategy_name(),
        });
        let excess = self.evaluations.len().saturating_sub(MAX_EVALUATIONS);
        self.evaluations.drain(..excess);
        let plateau = self.plateau();
        // Asked once per plateau: not again until the alternatives were
        // decided on, and a decision to continue gets as many evaluations
        // as a plateau takes before it is asked again.
        let decided = self.last_decided();
        Standing {
            plateau,
            wants_proposals: plateau
                && self.proposed_after <= decided
                && self.evaluated - decided >= PLATEAU,
        }
    }

    /// The evaluation `unchanged` repeats: the score of the last one, since a
    /// workspace nothing changed in scores what it scored.
    #[must_use]
    pub fn last_score(&self) -> Option<(u8, u32, u32)> {
        self.evaluations
            .last()
            .map(|evaluation| (evaluation.stage, evaluation.left, evaluation.residue))
    }

    /// The evaluations under the current strategy, oldest first.
    fn under_current(&self) -> Vec<&Evaluation> {
        let name = self.strategy_name();
        self.evaluations
            .iter()
            .filter(|evaluation| evaluation.strategy == name)
            .filter(|evaluation| evaluation.stage != stage::PASSED)
            .collect()
    }

    /// Whether the score has stopped improving: [`PLATEAU`] evaluations under
    /// the current strategy, and none since its best got further.
    #[must_use]
    pub fn plateau(&self) -> bool {
        let under = self.under_current();
        if under.len() < PLATEAU {
            return false;
        }
        let mut best = 0;
        for (index, evaluation) in under.iter().enumerate() {
            if evaluation.better_than(under[best]) {
                best = index;
            }
        }
        under.len() - best >= PLATEAU
    }

    fn last_decided(&self) -> usize {
        self.decisions.last().map_or(0, |decision| decision.after)
    }

    /// Whether an evaluation came after the last decision.
    #[must_use]
    pub fn decision_due(&self) -> bool {
        self.evaluated > self.last_decided()
            && self
                .evaluations
                .last()
                .is_some_and(|evaluation| evaluation.stage != stage::PASSED)
    }

    /// The model's decision after an evaluation.
    ///
    /// # Errors
    /// A switch names no strategy, or one that is the current strategy.
    pub fn decide(
        &mut self,
        kind: DecisionKind,
        to: Option<Strategy>,
        evidence: String,
    ) -> Result<StrategyDecision, String> {
        let from = self.strategy_name();
        let mut decision = StrategyDecision {
            kind,
            from: from.clone(),
            to: String::new(),
            evidence,
            after: self.evaluated,
            plateau: self.plateau(),
            proposed: false,
        };
        if kind == DecisionKind::Switch {
            let to = to.ok_or("task_state: a switch names the strategy it switches to")?;
            if same_name(&to.name, &from) {
                return Err(format!(
                    "task_state: {from:?} is the current strategy; continue with it, or switch to another"
                ));
            }
            decision.proposed = self
                .proposals
                .iter()
                .any(|proposal| same_name(&proposal.name, &to.name));
            decision.to.clone_from(&to.name);
            self.current = Some(to);
        }
        self.proposals.clear();
        self.decisions.push(decision.clone());
        let excess = self.decisions.len().saturating_sub(MAX_DECISIONS);
        self.decisions.drain(..excess);
        Ok(decision)
    }

    /// Name the strategy the task follows; not a switch when there was none.
    pub fn set(&mut self, strategy: Strategy) {
        self.current = Some(strategy);
    }

    pub fn write(&mut self, kind: JournalKind, text: String) {
        let strategy = self.strategy_name();
        self.journal.push(JournalEntry {
            kind,
            text,
            strategy,
        });
        let excess = self.journal.len().saturating_sub(MAX_JOURNAL);
        self.journal.drain(..excess);
    }

    /// The strategist's alternatives, kept until the next decision.
    pub fn propose(&mut self, proposals: Vec<Alternative>) {
        self.proposals = proposals;
        self.proposed_after = self.evaluated;
    }

    /// How often the task switched, and how often a switch took a named
    /// alternative: the paper's strategy-level measures.
    #[must_use]
    pub fn measures(&self) -> (usize, usize, usize) {
        let switches = self
            .decisions
            .iter()
            .filter(|decision| decision.kind == DecisionKind::Switch)
            .count();
        let proposed = self
            .decisions
            .iter()
            .filter(|decision| decision.proposed)
            .count();
        (self.decisions.len(), switches, proposed)
    }

    /// What every turn is shown.
    pub fn render(&self, text: &mut String) {
        if let Some(strategy) = &self.current {
            let _ = write!(text, "\nStrategy: {}", strategy.name);
            if !strategy.why.trim().is_empty() {
                let _ = write!(text, " (why: {})", strategy.why);
            }
            if !strategy.alternatives.is_empty() {
                let _ = write!(
                    text,
                    "\n  weighed against: {}",
                    strategy
                        .alternatives
                        .iter()
                        .map(|alternative| alternative.name.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
        if !self.journal.is_empty() {
            text.push_str("\nJournal (newest last):");
            let start = self.journal.len().saturating_sub(SHOWN_JOURNAL);
            for entry in &self.journal[start..] {
                let _ = write!(text, "\n- [{}] {}", entry.kind.label(), entry.text);
            }
        }
        let under = self.under_current();
        if !under.is_empty() {
            let _ = write!(
                text,
                "\nEvaluations under this strategy: {}",
                under
                    .iter()
                    .map(|evaluation| format!("{} {}", evaluation.source, evaluation.left))
                    .collect::<Vec<_>>()
                    .join(" -> ")
            );
        }
        if self.decision_due() {
            let last = self.evaluations.last().map_or("", |e| e.detail.as_str());
            let _ = write!(
                text,
                "\nDecision due: the work was evaluated and is not done ({last}). Before going on, weigh whether the strategy is still paying, then record continue or switch with task_state decide and the evidence. Continue when the last evaluations show it getting further; switch when they do not."
            );
            if self.plateau() {
                let _ = write!(
                    text,
                    "\nPLATEAU: {PLATEAU} or more evaluations under this strategy without getting further. More of the same is unlikely to finish the task: prefer a switch, and continue only with evidence that this strategy is about to succeed."
                );
            }
            if !self.proposals.is_empty() {
                text.push_str("\nAlternatives named by a strategist that read the task afresh:");
                for proposal in &self.proposals {
                    let _ = write!(text, "\n- {}: {}", proposal.name, proposal.why);
                }
            }
        }
    }

    /// One line for the recap a long turn recites.
    #[must_use]
    pub fn recap(&self) -> Option<String> {
        let name = self.current.as_ref().map(|strategy| strategy.name.as_str());
        match (name, self.decision_due()) {
            (Some(name), true) => Some(format!("Strategy: {name}; a decision is due")),
            (Some(name), false) => Some(format!("Strategy: {name}")),
            (None, true) => Some("A decision on the strategy is due".to_owned()),
            (None, false) => None,
        }
    }
}

/// `/strategy`: the strategy, its evaluations, and every decision on it.
#[must_use]
pub fn view(state: &StrategyState) -> Vec<String> {
    if state.is_empty() {
        return vec![
            "No strategy recorded for this task yet. A task the host evaluates - a goal, checks to pass - names its strategy with task_state.".to_owned(),
        ];
    }
    let mut lines = Vec::new();
    match &state.current {
        Some(strategy) if strategy.why.is_empty() => {
            lines.push(format!("Strategy: {}", strategy.name));
        }
        Some(strategy) => lines.push(format!("Strategy: {} - {}", strategy.name, strategy.why)),
        None => lines.push("Strategy: not named".to_owned()),
    }
    let (decisions, switches, proposed) = state.measures();
    lines.push(format!(
        "Evaluations: {}{}; decisions: {decisions}, switches: {switches} ({proposed} to a strategist's alternative)",
        state.evaluated,
        if state.plateau() { ", at a plateau" } else { "" }
    ));
    if !state.evaluations.is_empty() {
        lines.push(String::new());
        lines.push("Evaluations, oldest first:".to_owned());
        for evaluation in &state.evaluations {
            lines.push(format!(
                "  {:>3}. {} ({} left) under {}{}",
                evaluation.seq,
                evaluation.source,
                evaluation.left,
                if evaluation.strategy.is_empty() {
                    "no named strategy"
                } else {
                    &evaluation.strategy
                },
                if evaluation.detail.is_empty() {
                    String::new()
                } else {
                    format!(": {}", evaluation.detail)
                }
            ));
        }
    }
    if !state.decisions.is_empty() {
        lines.push(String::new());
        lines.push("Decisions:".to_owned());
        for decision in &state.decisions {
            let what = match decision.kind {
                DecisionKind::Continue => format!("continue {}", decision.from),
                DecisionKind::Switch => format!("switch {} -> {}", decision.from, decision.to),
            };
            lines.push(format!(
                "  after {}: {what}{} - {}",
                decision.after,
                if decision.plateau {
                    " (at a plateau)"
                } else {
                    ""
                },
                decision.evidence
            ));
        }
    }
    if !state.proposals.is_empty() {
        lines.push(String::new());
        lines.push("Alternatives the strategist named, not yet decided on:".to_owned());
        for proposal in &state.proposals {
            lines.push(format!("  - {}: {}", proposal.name, proposal.why));
        }
    }
    if !state.journal.is_empty() {
        lines.push(String::new());
        lines.push("Journal:".to_owned());
        for entry in &state.journal {
            lines.push(format!("  [{}] {}", entry.kind.label(), entry.text));
        }
    }
    lines
}

fn same_name(left: &str, right: &str) -> bool {
    left.trim().to_lowercase() == right.trim().to_lowercase()
}

fn clipped(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut cut = text.chars().take(limit).collect::<String>();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::{Alternative, DecisionKind, JournalKind, Strategy, StrategyState, stage};

    fn named(name: &str) -> Strategy {
        Strategy {
            name: name.to_owned(),
            why: String::new(),
            alternatives: Vec::new(),
        }
    }

    #[test]
    fn a_score_that_stops_improving_is_a_plateau() {
        let mut state = StrategyState::default();
        state.set(named("tune the regex"));
        assert!(
            !state
                .evaluate("checks", stage::CHECKS, 5, 0, "5 failed")
                .plateau
        );
        // Getting further is progress.
        assert!(
            !state
                .evaluate("checks", stage::CHECKS, 3, 0, "3 failed")
                .plateau
        );
        assert!(
            !state
                .evaluate("checks", stage::CHECKS, 3, 0, "3 failed")
                .plateau
        );
        let standing = state.evaluate("checks", stage::CHECKS, 4, 0, "4 failed");
        assert!(standing.plateau && standing.wants_proposals);
        assert!(state.decision_due());
    }

    #[test]
    fn a_later_stage_is_further_than_fewer_problems_at_an_earlier_one() {
        let mut state = StrategyState::default();
        state.evaluate("checklist", stage::CHECKLIST, 1, 0, "");
        state.evaluate("checks", stage::CHECKS, 9, 0, "");
        state.evaluate("verifier", stage::VERIFIER, 1, 0, "");
        assert!(!state.plateau(), "every evaluation got further");
    }

    #[test]
    fn a_switch_starts_the_count_again_and_records_a_named_proposal() {
        let mut state = StrategyState::default();
        state.set(named("tune the regex"));
        for _ in 0..3 {
            state.evaluate("checks", stage::CHECKS, 2, 0, "2 failed");
        }
        state.propose(vec![Alternative {
            name: "Use a parser".to_owned(),
            why: "the input nests".to_owned(),
        }]);
        let decision = state
            .decide(
                DecisionKind::Switch,
                Some(named("use a parser")),
                "plateau".to_owned(),
            )
            .expect("switched");
        assert!(decision.plateau && decision.proposed);
        assert!(!state.decision_due());
        assert!(
            !state.plateau(),
            "nothing is evaluated under the new strategy yet"
        );
        assert!(state.proposals.is_empty());
        assert_eq!(state.measures(), (1, 1, 1));
    }

    #[test]
    fn a_switch_needs_another_strategy() {
        let mut state = StrategyState::default();
        state.set(named("a"));
        assert!(
            state
                .decide(DecisionKind::Switch, None, String::new())
                .is_err()
        );
        assert!(
            state
                .decide(DecisionKind::Switch, Some(named("a")), String::new())
                .is_err()
        );
    }

    #[test]
    fn proposals_are_asked_for_once_per_plateau() {
        let mut state = StrategyState::default();
        for _ in 0..3 {
            state.evaluate("checks", stage::CHECKS, 2, 0, "");
        }
        state.propose(vec![]);
        assert!(
            !state
                .evaluate("checks", stage::CHECKS, 2, 0, "")
                .wants_proposals
        );
        state
            .decide(DecisionKind::Continue, None, "one more try".to_owned())
            .expect("continued");
        // A decision to continue gets a plateau's worth of evaluations
        // before the strategist is asked again.
        for _ in 0..2 {
            assert!(
                !state
                    .evaluate("checks", stage::CHECKS, 2, 0, "")
                    .wants_proposals
            );
        }
        assert!(
            state
                .evaluate("checks", stage::CHECKS, 2, 0, "")
                .wants_proposals
        );
    }

    /// One check failing every time is not a plateau when what it reports
    /// keeps shrinking: forty failing tests, then ten, then two.
    #[test]
    fn a_failure_that_shrinks_is_progress() {
        let mut state = StrategyState::default();
        for residue in [40, 10, 2] {
            state.evaluate("checks", stage::CHECKS, 1, residue, "");
        }
        assert!(!state.plateau());
        state.evaluate("checks", stage::CHECKS, 1, 2, "");
        state.evaluate("checks", stage::CHECKS, 1, 3, "");
        assert!(state.plateau(), "then it stopped shrinking");
    }

    #[test]
    fn the_decision_block_names_the_plateau_and_the_alternatives() {
        let mut state = StrategyState::default();
        state.set(named("tune the regex"));
        state.write(
            JournalKind::Lesson,
            "the regex cannot count brackets".to_owned(),
        );
        for _ in 0..3 {
            state.evaluate("checks", stage::CHECKS, 2, 0, "2 checks failed");
        }
        state.propose(vec![Alternative {
            name: "use a parser".to_owned(),
            why: "brackets nest".to_owned(),
        }]);
        let mut text = String::new();
        state.render(&mut text);
        assert!(text.contains("Strategy: tune the regex"), "{text}");
        assert!(
            text.contains("[lesson] the regex cannot count brackets"),
            "{text}"
        );
        assert!(text.contains("checks 2 -> checks 2 -> checks 2"), "{text}");
        assert!(text.contains("Decision due"), "{text}");
        assert!(text.contains("PLATEAU"), "{text}");
        assert!(text.contains("- use a parser: brackets nest"), "{text}");
    }
}
