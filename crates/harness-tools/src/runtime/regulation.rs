//! Trajectory regulation: what a turn's calls are doing, judged after each one.
//!
//! LIFE-HARNESS (arXiv 2605.22166) watches the trajectory after every step for
//! the patterns that waste a run - the same failure over and over, two states
//! visited in turn, a budget running out - and answers each with a note naming
//! what happened, at the call that showed it. Without it the paper's agents
//! lost most of their gains (86.5% of `ALFWorld`'s). ha's loop check saw only the
//! same call repeated; a model that failed with the same error twelve times,
//! each time with a slightly different path, was never told.
//!
//! Everything here is structural: a failure's class is its error code and its
//! message with the parts that change (numbers, paths, quoted text) left out,
//! and a repeat is the same class from the same tool. No word of a message is
//! looked up anywhere.

use std::collections::HashSet;
use std::fmt::Write as _;

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::contracts::{EffectClass, effect_class_for};

/// Failures of one tool with one class before the turn is told so.
pub(crate) const REPEAT_FAILURES: usize = 3;

/// Failures of one class after which the note asks the model to stop that
/// kind of call outright.
pub(crate) const STOP_FAILURES: usize = 5;

/// How many alternating calls make an oscillation: A, B, A, B.
const OSCILLATION: usize = 4;

/// What one settled call left behind.
#[derive(Clone, Debug)]
struct Settled {
    tool: String,
    signature: String,
    effect: EffectClass,
    failure: Option<Failure>,
}

/// How a call failed, as a class that ignores what changes between attempts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Failure {
    /// The error code, `invalid_call` for a call that never reached a tool, or
    /// `denied` for a policy or guard refusal.
    pub(crate) code: String,
    pub(crate) message: String,
    /// `code:hash` of the message without its numbers, paths and quotes.
    pub(crate) class: String,
    /// The harness or the user refused the call (a repeat answered without
    /// running, a learned rule, an approval or policy): not a failure of the
    /// call itself, so neither a repeat nor evidence for the evolution.
    pub(crate) refused: bool,
}

impl Failure {
    pub(crate) fn new(code: &str, message: &str) -> Self {
        let message = first_line(message);
        Self {
            code: code.to_owned(),
            class: format!("{code}:{}", short_hash(&shape_of_message(&message))),
            refused: REFUSALS.contains(&code),
            message,
        }
    }

    /// A refusal the harness made itself.
    pub(crate) fn refusal(message: &str) -> Self {
        Self {
            refused: true,
            ..Self::new("refused", message)
        }
    }
}

/// Codes that refuse a call rather than fail it: an approval, a policy, a
/// hook, the harness.
const REFUSALS: [&str; 7] = [
    "refused",
    "policy_denied",
    "approval_required",
    "approval_stale",
    "approval_consumed",
    "approval_revoked",
    "blocked_by_hook",
];

/// Codes of a failure that the same call may not meet again: the workspace,
/// a lease or the provider moved on, not the call.
const TRANSIENT: [&str; 9] = [
    "stale_workspace",
    "revision_conflict",
    "task_lease_conflict",
    "writer_locked",
    "rate_limited",
    "service_unavailable",
    "provider_canceled",
    "process_timed_out",
    "process_canceled",
];

/// What the regulator wants the model to read, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Intervention {
    /// `repeated_failure`, `stop_failing`, `oscillation` or `budget`.
    pub(crate) kind: &'static str,
    pub(crate) note: String,
}

/// One turn's trajectory, as the regulator sees it.
#[derive(Debug, Default)]
pub(crate) struct Regulator {
    calls: Vec<Settled>,
    /// `(tool, class, level)` already noted, so a note is said once per level.
    noted: HashSet<(String, String, usize)>,
    oscillations: HashSet<(String, String)>,
    budget_noted: bool,
    /// The user steered since the last call: what they said may be what the
    /// same call was waiting for.
    steered: bool,
}

impl Regulator {
    /// A call that would fail as it failed just before: the same tool with the
    /// same arguments as the call right before it, which failed. Nothing ran
    /// between them and the user said nothing, so nothing could have changed
    /// the answer - a read in between can (a write retried after it carries
    /// the hash the read gave), and so can a process still running or a reply
    /// the user sent. Only tools whose answer depends on the workspace alone
    /// are judged, and only failures of the call itself: a refused approval,
    /// a stale workspace or a busy provider is not the call's fault.
    pub(crate) fn repeat_of_failure(&self, tool: &str, signature: &str) -> Option<String> {
        if !deterministic(tool) || self.steered {
            return None;
        }
        let last = self.calls.last()?;
        let failure = last.failure.as_ref()?;
        if last.signature != signature
            || failure.refused
            || TRANSIENT.contains(&failure.code.as_str())
        {
            return None;
        }
        Some(format!(
            "not executed: this exact {tool} call failed just before ({}: {}), and nothing has run or been said since, so it would fail the same way. Change the arguments, or first do what the error asks.",
            failure.code, failure.message
        ))
    }

    /// The user's message reached the turn between calls.
    pub(crate) fn steer(&mut self) {
        self.steered = true;
    }

    /// Record a settled call and say what, if anything, the trajectory shows.
    pub(crate) fn settle(
        &mut self,
        tool: &str,
        signature: &str,
        failure: Option<Failure>,
    ) -> Option<Intervention> {
        self.steered = false;
        // A refusal is the harness's or the user's answer, not something the
        // call did: it neither repeats a failure nor counts toward one.
        if failure.as_ref().is_some_and(|failure| failure.refused) {
            return None;
        }
        self.calls.push(Settled {
            tool: tool.to_owned(),
            signature: signature.to_owned(),
            effect: effect_class_for(tool),
            failure,
        });
        self.repeated_failure().or_else(|| self.oscillation())
    }

    /// A note once the turn's remaining steps or calls are few: a long turn
    /// that meets its limit ends with nothing said.
    pub(crate) fn budget(
        &mut self,
        steps: u32,
        max_steps: u32,
        calls: u32,
        max_calls: u32,
    ) -> Option<Intervention> {
        if self.budget_noted {
            return None;
        }
        let left = |used: u32, limit: u32| limit.saturating_sub(used);
        let near = |used: u32, limit: u32| {
            // A tenth of the budget, and never fewer than two: a short budget
            // has no room for a note given at its very last step.
            limit >= 10 && left(used, limit) <= (limit / 10).max(2)
        };
        if !(near(steps, max_steps) || near(calls, max_calls)) {
            return None;
        }
        self.budget_noted = true;
        Some(Intervention {
            kind: "budget",
            note: format!(
                "[harness: this turn has {} step(s) and {} tool call(s) left. Finish what is in hand, record where the task stands, and answer; do not start anything new.]",
                left(steps, max_steps),
                left(calls, max_calls)
            ),
        })
    }

    fn repeated_failure(&mut self) -> Option<Intervention> {
        let last = self.calls.last()?;
        let failure = last.failure.clone()?;
        let tool = last.tool.clone();
        // Failures of this class from this tool since its last success.
        let mut same = Vec::new();
        for call in self.calls.iter().rev().filter(|call| call.tool == tool) {
            match &call.failure {
                Some(other) if other.class == failure.class => same.push(call),
                Some(_) => {}
                None => break,
            }
        }
        let count = same.len();
        let level = if count >= STOP_FAILURES {
            STOP_FAILURES
        } else if count >= REPEAT_FAILURES {
            REPEAT_FAILURES
        } else {
            return None;
        };
        if !self
            .noted
            .insert((tool.clone(), failure.class.clone(), level))
        {
            return None;
        }
        let distinct = same
            .iter()
            .map(|call| call.signature.as_str())
            .collect::<HashSet<_>>()
            .len();
        let tried = if distinct > 1 {
            format!(" with {distinct} different sets of arguments")
        } else {
            String::new()
        };
        Some(if level == STOP_FAILURES {
            Intervention {
                kind: "stop_failing",
                note: format!(
                    "[harness: {tool} has now failed {count} times in this turn{tried}, every time with the same error ({}: {}). Variations of this call are not working. Stop making it: either take a different approach to the step, or say plainly what blocks you and why.]",
                    failure.code, failure.message
                ),
            }
        } else {
            Intervention {
                kind: "repeated_failure",
                note: format!(
                    "[harness: {tool} has failed {count} times in this turn{tried}, every time with the same error ({}: {}). Before calling it again, read what the error says is wrong and fix that, or use another way to the same end.]",
                    failure.code, failure.message
                ),
            }
        })
    }

    fn oscillation(&mut self) -> Option<Intervention> {
        // Edits, newest last: two that undo each other are an oscillation.
        // Reads and commands move between states without leaving one - a
        // test run between edits, or polling, is not undoing anything.
        let acting = self
            .calls
            .iter()
            .filter(|call| call.effect == EffectClass::Mutating)
            .collect::<Vec<_>>();
        if acting.len() < OSCILLATION {
            return None;
        }
        let tail = &acting[acting.len() - OSCILLATION..];
        let (a, b) = (&tail[0].signature, &tail[1].signature);
        let alternates = a != b
            && tail
                .iter()
                .enumerate()
                .all(|(index, call)| &call.signature == if index % 2 == 0 { a } else { b });
        if !alternates {
            return None;
        }
        let key = if a < b {
            (a.clone(), b.clone())
        } else {
            (b.clone(), a.clone())
        };
        if !self.oscillations.insert(key) {
            return None;
        }
        Some(Intervention {
            kind: "oscillation",
            note: format!(
                "[harness: the last {OSCILLATION} actions alternated between two calls ({} and {}), each undoing or redoing the other. Settle which state is right before acting again.]",
                tail[0].tool, tail[1].tool
            ),
        })
    }
}

/// Whether a tool's answer depends on the workspace alone, so that the same
/// call with nothing changed fails the same way.
fn deterministic(tool: &str) -> bool {
    match effect_class_for(tool) {
        EffectClass::ReadOnly => !matches!(
            tool,
            "read_process_output" | "history_search" | "history_read"
        ),
        EffectClass::Mutating => true,
        EffectClass::External | EffectClass::Interactive => false,
    }
}

/// The names and kinds of a call's arguments, without their values: what a
/// call looked like, safe to keep in a diagnostic record.
pub(crate) fn arguments_shape(arguments: &str) -> String {
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(arguments) else {
        return "unparsed".to_owned();
    };
    let mut parts = map
        .iter()
        .map(|(key, value)| format!("{key}:{}", kind_of(value)))
        .collect::<Vec<_>>();
    parts.sort();
    parts.join(",")
}

fn kind_of(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(_) => "bool".to_owned(),
        Value::Number(_) => "number".to_owned(),
        Value::String(_) => "string".to_owned(),
        Value::Array(_) => "array".to_owned(),
        Value::Object(_) => "object".to_owned(),
    }
}

/// A short, stable hash of a call's arguments, with the keys in order so the
/// same arguments written in another order hash the same.
pub(crate) fn arguments_hash(arguments: &str) -> String {
    let canonical = serde_json::from_str::<Value>(arguments).map_or_else(
        |_| arguments.to_owned(),
        |value| canonical(&value).to_string(),
    );
    short_hash(&canonical)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn short_hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    digest[..6].iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// A message with what changes between attempts left out: a token holding a
/// digit, a path separator, a quote, or a dot inside it (`foo.rs`, not the
/// full stop ending a sentence) is a value, not part of what went wrong.
fn shape_of_message(message: &str) -> String {
    message
        .split_whitespace()
        .map(|token| {
            let inner = token.trim_end_matches(['.', ',', ':', ';']);
            if inner.contains('.')
                || token
                    .chars()
                    .any(|c| c.is_ascii_digit() || matches!(c, '/' | '\\' | '"' | '\'' | '`'))
            {
                "_"
            } else {
                token
            }
        })
        .take(24)
        .collect::<Vec<_>>()
        .join(" ")
}

fn first_line(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim();
    let mut end = line.len().min(240);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::{Failure, Regulator, arguments_hash, arguments_shape};

    #[allow(
        clippy::unnecessary_wraps,
        reason = "settle takes a call's failure as an Option"
    )]
    fn missing(path: &str) -> Option<Failure> {
        Some(Failure::new(
            "invalid_payload",
            &format!("File not found: {path}"),
        ))
    }

    #[test]
    fn the_same_error_with_different_paths_is_one_failure_repeated() {
        let mut regulator = Regulator::default();
        assert!(
            regulator
                .settle("read_file", "read_file|a", missing("src/a.rs"))
                .is_none()
        );
        assert!(
            regulator
                .settle("read_file", "read_file|b", missing("src/net/a.rs"))
                .is_none()
        );
        let third = regulator
            .settle("read_file", "read_file|c", missing("lib/a.rs"))
            .expect("noted");
        assert_eq!(third.kind, "repeated_failure");
        assert!(
            third.note.contains("3 different sets of arguments"),
            "{}",
            third.note
        );
        // Said once at that level, then once more when it is time to stop.
        assert!(
            regulator
                .settle("read_file", "read_file|d", missing("x/a.rs"))
                .is_none()
        );
        let fifth = regulator
            .settle("read_file", "read_file|e", missing("y/a.rs"))
            .expect("noted");
        assert_eq!(fifth.kind, "stop_failing");
    }

    #[test]
    fn a_success_between_failures_starts_the_count_again() {
        let mut regulator = Regulator::default();
        regulator.settle("read_file", "read_file|a", missing("a"));
        regulator.settle("read_file", "read_file|b", missing("b"));
        regulator.settle("read_file", "read_file|ok", None);
        assert!(
            regulator
                .settle("read_file", "read_file|c", missing("c"))
                .is_none()
        );
    }

    #[test]
    fn a_call_that_just_failed_is_not_run_again_unchanged() {
        let mut regulator = Regulator::default();
        regulator.settle("edit_file", "edit_file|x", missing("a.rs"));
        let refusal = regulator
            .repeat_of_failure("edit_file", "edit_file|x")
            .expect("refused");
        assert!(refusal.contains("failed just before"), "{refusal}");
        // Different arguments are another call.
        assert!(
            regulator
                .repeat_of_failure("edit_file", "edit_file|y")
                .is_none()
        );
        // The refusal itself is not a failure: the next identical call is
        // still the repeat of the one that failed.
        regulator.settle(
            "edit_file",
            "edit_file|x",
            Some(Failure::refusal("not executed")),
        );
        assert!(
            regulator
                .repeat_of_failure("edit_file", "edit_file|x")
                .is_some()
        );
    }

    /// A write refused for a stale hash succeeds once a read gives the hash:
    /// any call between them, a read included, lets the retry run.
    #[test]
    fn a_retry_after_another_call_or_the_users_word_runs() {
        let mut regulator = Regulator::default();
        regulator.settle("write_file", "write|x", missing("a.rs"));
        regulator.settle("read_file", "read|a", None);
        assert!(
            regulator
                .repeat_of_failure("write_file", "write|x")
                .is_none()
        );
        let mut regulator = Regulator::default();
        regulator.settle("write_file", "write|x", missing("a.rs"));
        regulator.steer();
        assert!(
            regulator
                .repeat_of_failure("write_file", "write|x")
                .is_none()
        );
        // A failure that is not the call's own - a stale workspace, a refused
        // approval - is never a reason to refuse it.
        let mut regulator = Regulator::default();
        regulator.settle(
            "write_file",
            "write|x",
            Some(Failure::new("stale_workspace", "pass the hash")),
        );
        assert!(
            regulator
                .repeat_of_failure("write_file", "write|x")
                .is_none()
        );
        regulator.settle(
            "write_file",
            "write|y",
            Some(Failure::new("approval_stale", "the approval expired")),
        );
        assert!(
            regulator
                .repeat_of_failure("write_file", "write|y")
                .is_none()
        );
    }

    #[test]
    fn a_file_name_is_a_value_in_an_error() {
        assert_eq!(
            Failure::new("invalid_payload", "File not found: foo.rs").class,
            Failure::new("invalid_payload", "File not found: bar.rs").class
        );
        assert_ne!(
            Failure::new("invalid_payload", "File not found.").class,
            Failure::new("invalid_payload", "Path is a directory.").class
        );
    }

    #[test]
    fn a_command_is_never_refused_for_failing_before() {
        let mut regulator = Regulator::default();
        regulator.settle("run_shell", "run_shell|curl", missing("x"));
        assert!(
            regulator
                .repeat_of_failure("run_shell", "run_shell|curl")
                .is_none()
        );
    }

    #[test]
    fn two_edits_undoing_each_other_are_an_oscillation() {
        let mut regulator = Regulator::default();
        assert!(regulator.settle("edit_file", "edit|a", None).is_none());
        // A read between them changes nothing.
        assert!(regulator.settle("read_file", "read|f", None).is_none());
        assert!(regulator.settle("edit_file", "edit|b", None).is_none());
        assert!(regulator.settle("edit_file", "edit|a", None).is_none());
        let noted = regulator
            .settle("edit_file", "edit|b", None)
            .expect("noted");
        assert_eq!(noted.kind, "oscillation");
        assert!(
            regulator.settle("edit_file", "edit|a", None).is_none(),
            "said once"
        );
    }

    #[test]
    fn the_budget_is_noted_once_near_its_end() {
        let mut regulator = Regulator::default();
        assert!(regulator.budget(10, 100, 10, 200).is_none());
        let note = regulator.budget(90, 100, 10, 200).expect("noted");
        assert!(note.note.contains("10 step(s)"), "{}", note.note);
        assert!(regulator.budget(95, 100, 10, 200).is_none());
        // A short budget has no tenth to give.
        assert!(Regulator::default().budget(7, 8, 0, 16).is_none());
    }

    #[test]
    fn arguments_are_kept_as_their_shape_and_hash() {
        assert_eq!(
            arguments_shape(r#"{"path":"a","edits":[1,2],"n":3}"#),
            "edits:array,n:number,path:string"
        );
        assert_eq!(
            arguments_hash(r#"{"a":1,"b":2}"#),
            arguments_hash(r#"{"b":2,"a":1}"#)
        );
    }
}
