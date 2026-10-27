//! Rules learned from a project's own failures, applied before a call runs.
//!
//! LIFE-HARNESS (arXiv 2605.22166) evolves an action realization layer from
//! training trajectories: checks that canonicalize or block a call that would
//! deterministically fail. Its rules are code an evolution model writes; ha's
//! are data in a small vocabulary that cannot do anything else:
//! - `rename_argument`: a call that names an argument the tool does not take
//!   (`file` for `path`) is given the one it does;
//! - `refuse_shape`: a call whose arguments have a shape that has only ever
//!   failed, with one error, is answered with what to do instead.
//!
//! A rule starts in shadow: it records that it would have acted and how the
//! call then went, and acts only once the host promoted it on that evidence -
//! the paper keeps a change only when it measurably helps.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a learned rule does.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleAction {
    RenameArgument { from: String, to: String },
    RefuseShape { shape: String, reason: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleState {
    #[default]
    Shadow,
    Active,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LearnedRule {
    pub id: String,
    pub tool: String,
    #[serde(flatten)]
    pub action: RuleAction,
    #[serde(default)]
    pub state: RuleState,
}

/// What the rules made of one call.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Realized {
    /// The arguments to run with, when a rule changed them.
    pub arguments: Option<String>,
    /// Why the call is not run, when a rule refused it.
    pub refusal: Option<String>,
    /// Rules that acted.
    pub acted: Vec<String>,
    /// Rules in shadow that would have acted.
    pub shadowed: Vec<String>,
}

/// The rules a turn applies.
#[derive(Clone, Debug, Default)]
pub struct LearnedRules {
    rules: Vec<LearnedRule>,
}

impl LearnedRules {
    #[must_use]
    pub fn new(rules: Vec<LearnedRule>) -> Self {
        Self {
            rules: rules
                .into_iter()
                .filter(|rule| rule.state != RuleState::Retired)
                .collect(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Apply the rules for `tool` to a call's arguments.
    #[must_use]
    pub fn realize(&self, tool: &str, arguments: &str) -> Realized {
        let mut realized = Realized::default();
        let Ok(Value::Object(mut map)) = serde_json::from_str::<Value>(arguments) else {
            return realized;
        };
        let mut changed = false;
        for rule in self.rules.iter().filter(|rule| rule.tool == tool) {
            let applies = match &rule.action {
                RuleAction::RenameArgument { from, to } => {
                    map.contains_key(from) && !map.contains_key(to)
                }
                RuleAction::RefuseShape { shape, .. } => {
                    crate::regulation::arguments_shape(arguments) == *shape
                }
            };
            if !applies {
                continue;
            }
            if rule.state == RuleState::Shadow {
                realized.shadowed.push(rule.id.clone());
                continue;
            }
            realized.acted.push(rule.id.clone());
            match &rule.action {
                RuleAction::RenameArgument { from, to } => {
                    if let Some(value) = map.remove(from) {
                        map.insert(to.clone(), value);
                        changed = true;
                    }
                }
                RuleAction::RefuseShape { reason, .. } => {
                    realized.refusal = Some(format!(
                        "not executed: calls to {tool} with these arguments ({}) have always failed in this project. {reason}",
                        crate::regulation::arguments_shape(arguments)
                    ));
                    return realized;
                }
            }
        }
        if changed {
            realized.arguments = Some(Value::Object(map).to_string());
        }
        realized
    }
}

#[cfg(test)]
mod tests {
    use super::{LearnedRule, LearnedRules, RuleAction, RuleState};

    fn rule(id: &str, action: RuleAction, state: RuleState) -> LearnedRule {
        LearnedRule {
            id: id.to_owned(),
            tool: "read_file".to_owned(),
            action,
            state,
        }
    }

    #[test]
    fn an_active_rename_gives_the_argument_the_tool_takes() {
        let rules = LearnedRules::new(vec![rule(
            "r1",
            RuleAction::RenameArgument {
                from: "file".to_owned(),
                to: "path".to_owned(),
            },
            RuleState::Active,
        )]);
        let realized = rules.realize("read_file", r#"{"file":"a.rs"}"#);
        assert_eq!(realized.arguments.as_deref(), Some(r#"{"path":"a.rs"}"#));
        assert_eq!(realized.acted, ["r1"]);
        // A call that already names it, or another tool, is left alone.
        assert!(
            rules
                .realize("read_file", r#"{"path":"a"}"#)
                .acted
                .is_empty()
        );
        assert!(
            rules
                .realize("list_files", r#"{"file":"a"}"#)
                .acted
                .is_empty()
        );
    }

    #[test]
    fn a_rule_in_shadow_only_says_it_would_have_acted() {
        let rules = LearnedRules::new(vec![rule(
            "r2",
            RuleAction::RefuseShape {
                shape: "file:string".to_owned(),
                reason: "Pass path.".to_owned(),
            },
            RuleState::Shadow,
        )]);
        let realized = rules.realize("read_file", r#"{"file":"a.rs"}"#);
        assert!(realized.refusal.is_none() && realized.arguments.is_none());
        assert_eq!(realized.shadowed, ["r2"]);
    }

    #[test]
    fn an_active_refusal_answers_without_running() {
        let rules = LearnedRules::new(vec![
            rule(
                "r3",
                RuleAction::RefuseShape {
                    shape: "file:string".to_owned(),
                    reason: "Pass path.".to_owned(),
                },
                RuleState::Active,
            ),
            rule(
                "old",
                RuleAction::RefuseShape {
                    shape: "path:string".to_owned(),
                    reason: String::new(),
                },
                RuleState::Retired,
            ),
        ]);
        let refusal = rules
            .realize("read_file", r#"{"file":"a.rs"}"#)
            .refusal
            .expect("refused");
        assert!(refusal.ends_with("Pass path."), "{refusal}");
        assert!(
            rules
                .realize("read_file", r#"{"path":"a"}"#)
                .refusal
                .is_none(),
            "retired"
        );
    }
}
