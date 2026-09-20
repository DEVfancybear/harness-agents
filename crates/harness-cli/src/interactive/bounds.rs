//! How long one turn may run, and what happens when a bound stops it.
//!
//! The bounds are a safety net, not the task's budget. They exist so a loop that has
//! gone wrong cannot spend a context window unattended — which is why a run that hits
//! one *pauses* instead of failing, and why the app continues it by itself while the
//! stop was only a count of work.
//!
//! Four variables move them, all optional:
//!
//! | Variable | Default | What it bounds |
//! | --- | --- | --- |
//! | `HA_TURN_MAX_STEPS` | 8 | Model calls in one turn. |
//! | `HA_TURN_MAX_TOOL_CALLS` | 16 | Tool calls in one turn. |
//! | `HA_TURN_DEADLINE_SECONDS` | 600 | Wall-clock seconds in one turn. |
//! | `HA_TURN_CONTINUATIONS` | 4 | Turns the app may continue on its own after a step or tool-call bound. |
//!
//! Only a positive integer counts for the three bounds. Anything else keeps the
//! default, because a misspelled value must not silently remove the bound that keeps an
//! agent from running away: `HA_TURN_MAX_STEPS=unlimited` is a typo, not a policy.
//! `HA_TURN_CONTINUATIONS` also accepts `0`, which turns automatic continuation off.

use std::time::Duration;

use harness_tools::TurnLimits;

use super::paths::LaunchEnvironment;

/// Steps one turn may take before it pauses.
pub const MAX_STEPS_VARIABLE: &str = "HA_TURN_MAX_STEPS";

/// Tool calls one turn may make before it pauses.
pub const MAX_TOOL_CALLS_VARIABLE: &str = "HA_TURN_MAX_TOOL_CALLS";

/// Wall-clock seconds one turn may take before it pauses.
pub const DEADLINE_VARIABLE: &str = "HA_TURN_DEADLINE_SECONDS";

/// Turns the app may continue by itself after a step or tool-call bound.
pub const CONTINUATIONS_VARIABLE: &str = "HA_TURN_CONTINUATIONS";

/// How many times one request is continued automatically.
///
/// Four continuations of the default eight steps is thirty-two model calls: enough for
/// real agentic work, still bounded, and the pause after them is a real pause.
pub const DEFAULT_CONTINUATIONS: u32 = 4;

/// A positive integer, or the default when the value is absent or unusable.
///
/// Zero is refused as well as text: a bound of zero would end every turn before the
/// model was called, which reads as a broken app rather than as a limit.
fn positive(value: Option<&str>, fallback: u32) -> u32 {
    value
        .and_then(|value| value.trim().parse::<u32>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(fallback)
}

/// A count that may be zero, or the default when the value is absent or unusable.
///
/// Zero is a real answer here, unlike a bound: `HA_TURN_CONTINUATIONS=0` means the app
/// never continues a turn by itself, which is the conservative behaviour an operator may
/// want back.
fn count(value: Option<&str>, fallback: u32) -> u32 {
    value
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(fallback)
}

/// The bounds one turn runs under, as the environment asks for them.
#[must_use]
pub fn limits_from_environment(environment: &LaunchEnvironment) -> TurnLimits {
    let defaults = TurnLimits::default();
    let deadline_seconds = positive(
        environment
            .value(DEADLINE_VARIABLE)
            .and_then(|value| value.to_str()),
        defaults.deadline.as_secs().try_into().unwrap_or(u32::MAX),
    );
    TurnLimits {
        max_steps: positive(
            environment
                .value(MAX_STEPS_VARIABLE)
                .and_then(|value| value.to_str()),
            defaults.max_steps,
        ),
        max_tool_calls: positive(
            environment
                .value(MAX_TOOL_CALLS_VARIABLE)
                .and_then(|value| value.to_str()),
            defaults.max_tool_calls,
        ),
        deadline: Duration::from_secs(u64::from(deadline_seconds)),
    }
}

/// How many automatic continuations one request may use.
#[must_use]
pub fn continuations_from_environment(environment: &LaunchEnvironment) -> u32 {
    count(
        environment
            .value(CONTINUATIONS_VARIABLE)
            .and_then(|value| value.to_str()),
        DEFAULT_CONTINUATIONS,
    )
}

/// One line for `/status`: the bounds in force, and what a pause would do.
#[must_use]
pub fn describe(limits: &TurnLimits, continuations: u32) -> String {
    let continuation = if continuations == 0 {
        "automatic continuation off".to_owned()
    } else {
        format!("up to {continuations} automatic continuation(s)")
    };
    format!(
        "Bounds:  {} steps, {} tool calls, {} s per turn; {continuation}",
        limits.max_steps,
        limits.max_tool_calls,
        limits.deadline.as_secs(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CONTINUATIONS_VARIABLE, DEADLINE_VARIABLE, DEFAULT_CONTINUATIONS, MAX_STEPS_VARIABLE,
        MAX_TOOL_CALLS_VARIABLE, continuations_from_environment, describe, limits_from_environment,
    };
    use crate::interactive::paths::LaunchEnvironment;

    fn environment(pairs: &[(&str, &str)]) -> LaunchEnvironment {
        LaunchEnvironment::from_pairs(pairs.iter().map(|(name, value)| (*name, *value)))
    }

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let limits = limits_from_environment(&environment(&[]));
        assert_eq!(limits.max_steps, 8);
        assert_eq!(limits.max_tool_calls, 16);
        assert_eq!(limits.deadline.as_secs(), 600);
        assert_eq!(
            continuations_from_environment(&environment(&[])),
            DEFAULT_CONTINUATIONS
        );
    }

    #[test]
    fn a_typed_variable_moves_its_bound() {
        let limits = limits_from_environment(&environment(&[
            (MAX_STEPS_VARIABLE, " 24 "),
            (MAX_TOOL_CALLS_VARIABLE, "48"),
            (DEADLINE_VARIABLE, "1800"),
        ]));
        assert_eq!(limits.max_steps, 24);
        assert_eq!(limits.max_tool_calls, 48);
        assert_eq!(limits.deadline.as_secs(), 1800);
        assert_eq!(
            continuations_from_environment(&environment(&[(CONTINUATIONS_VARIABLE, "1")])),
            1
        );
    }

    /// A bound that reads as text must not become "no bound at all".
    #[test]
    fn a_value_that_is_not_a_positive_number_keeps_the_default() {
        for value in ["", "0", "-1", "unlimited", "8 steps", "1e3"] {
            let limits = limits_from_environment(&environment(&[
                (MAX_STEPS_VARIABLE, value),
                (MAX_TOOL_CALLS_VARIABLE, value),
                (DEADLINE_VARIABLE, value),
            ]));
            assert_eq!(limits.max_steps, 8, "HA_TURN_MAX_STEPS={value}");
            assert_eq!(limits.max_tool_calls, 16, "HA_TURN_MAX_TOOL_CALLS={value}");
            assert_eq!(
                limits.deadline.as_secs(),
                600,
                "{DEADLINE_VARIABLE}={value}"
            );
        }
    }

    /// Turning the app's own continuations off is a real choice, not a typo: it restores
    /// the behaviour where every bound waits for the user.
    #[test]
    fn zero_continuations_is_off_and_a_typo_is_the_default() {
        assert_eq!(
            continuations_from_environment(&environment(&[(CONTINUATIONS_VARIABLE, "0")])),
            0
        );
        for value in ["", "-1", "off", "many"] {
            assert_eq!(
                continuations_from_environment(&environment(&[(CONTINUATIONS_VARIABLE, value)])),
                DEFAULT_CONTINUATIONS,
                "{CONTINUATIONS_VARIABLE}={value}"
            );
        }
        assert!(
            describe(&limits_from_environment(&environment(&[])), 0)
                .contains("automatic continuation off")
        );
    }

    #[test]
    fn the_status_line_names_every_bound() {
        let limits = limits_from_environment(&environment(&[]));
        let line = describe(&limits, 4);
        assert!(line.contains("8 steps"), "{line}");
        assert!(line.contains("16 tool calls"), "{line}");
        assert!(line.contains("600 s"), "{line}");
        assert!(line.contains("4 automatic continuation"), "{line}");
    }
}
