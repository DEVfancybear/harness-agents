//! The action bank switch: `HarnessMM`'s guarded decision reuse, off by default.
//!
//! On, the model may queue calls it has already fully decided (`action_bank`),
//! and a ready one runs without a model step (`harness_tools::bank`,
//! `docs/architecture/ACTION_BANK.en.md`). The paper's negative results - banks
//! rarely usable cost more to author than they save - are why it is off until
//! turned on: `/bank on`, settings.json `"actionBank": true`, or
//! `HA_ACTION_BANK=on`, which wins over the setting either way. Children run
//! with their parent's setting.

use std::path::Path;
use std::sync::Arc;

use super::paths::LaunchEnvironment;

/// The settings.json key.
pub const SETTING: &str = "actionBank";
/// The environment switch: `on` or `off` (also `1`/`0`, `true`/`false`,
/// `yes`/`no`); it overrides the setting.
pub const VARIABLE: &str = "HA_ACTION_BANK";

fn parse(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

/// Whether the bank is on: `HA_ACTION_BANK` when it says, else the setting,
/// else off.
#[must_use]
pub fn enabled(config_file: &Path, environment: &LaunchEnvironment) -> bool {
    environment
        .value(VARIABLE)
        .and_then(|value| value.to_str())
        .and_then(parse)
        .or_else(|| {
            super::config::load_setting(config_file, SETTING).and_then(|setting| setting.as_bool())
        })
        .unwrap_or(false)
}

/// Save the setting, and say what it now is.
///
/// # Errors
/// The settings file cannot be written.
pub fn set(
    config_file: &Path,
    environment: &LaunchEnvironment,
    enable: Option<bool>,
) -> Result<String, String> {
    if let Some(enable) = enable {
        super::config::save_setting(config_file, SETTING, Some(serde_json::json!(enable)))?;
    }
    let on = enabled(config_file, environment);
    harness_types::diag::info("bank", "status")
        .field("enabled", on)
        .emit();
    Ok(status(config_file, environment))
}

/// `/bank status`.
#[must_use]
pub fn status(config_file: &Path, environment: &LaunchEnvironment) -> String {
    let forced = environment
        .value(VARIABLE)
        .and_then(|value| value.to_str())
        .and_then(parse)
        .map(|on| {
            format!(
                " {VARIABLE}={} overrides the setting.",
                if on { "on" } else { "off" }
            )
        })
        .unwrap_or_default();
    if enabled(config_file, environment) {
        format!(
            "Action bank: on (settings.json {SETTING}). The model may queue calls it has fully decided (write, build, test), and a ready one runs without a model step; with jev on, its `bank` decision picks among several. /bank off turns it off.{forced}"
        )
    } else {
        format!(
            "Action bank: off (the default). /bank on lets the model queue calls it has fully decided, so a ready one runs without a model step; it saves model calls only when banks are used, so measure it on your tasks.{forced}"
        )
    }
}

/// The bank for a driver: the jev `bank` decision as its selector when jev
/// decides it, else none (several ready nodes then go to the model).
#[must_use]
pub fn bank(decisions: Option<&Arc<super::jev::JevDecisions>>) -> harness_tools::ActionBank {
    harness_tools::ActionBank {
        selector: decisions
            .filter(|decisions| decisions.decides("bank"))
            .map(|decisions| Arc::clone(decisions) as Arc<dyn harness_tools::BankSelector>),
    }
}

/// The bank as `ha exec --json` reports it.
#[must_use]
pub fn report(on: bool, tally: &harness_tools::BankTally) -> serde_json::Value {
    if !on {
        return serde_json::json!({"enabled": false});
    }
    serde_json::json!({
        "enabled": true,
        "authored": tally.authored,
        "refused": tally.refused,
        "nodes": tally.nodes,
        "dispatched": tally.dispatched,
        "model_steps_saved": tally.dispatched,
        "immediate": tally.immediate,
        "abstained": tally.abstained,
        "discarded": tally.discarded,
        "selector_calls": tally.selector_calls,
        "selector_input_tokens": tally.selector_input_tokens,
        "author_tokens_estimate": tally.author_tokens_estimate,
    })
}

/// What the bank did in each conversation of this process, by task id: what
/// `/context` sums up.
fn totals() -> &'static std::sync::Mutex<std::collections::HashMap<String, harness_tools::BankTally>>
{
    static TOTALS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, harness_tools::BankTally>>,
    > = std::sync::OnceLock::new();
    TOTALS.get_or_init(Default::default)
}

/// Add one turn's tally to its conversation's.
pub fn record(task: &str, tally: &harness_tools::BankTally) {
    if tally.is_empty() {
        return;
    }
    if let Ok(mut totals) = totals().lock() {
        totals.entry(task.to_owned()).or_default().add(tally);
    }
}

/// What the bank did in this conversation so far.
#[must_use]
pub fn total(task: &str) -> harness_tools::BankTally {
    totals()
        .lock()
        .ok()
        .and_then(|totals| totals.get(task).copied())
        .unwrap_or_default()
}

/// One line for `/context` and the turn's usage, when the bank did anything.
#[must_use]
pub fn line(tally: &harness_tools::BankTally) -> Option<String> {
    (!tally.is_empty()).then(|| {
        format!(
            "action bank: {} bank(s) of {} node(s), {} run without a model step ({} more in place of a call of its own), {} back to the model, {} dropped; authoring ~{} tokens (estimate){}",
            tally.authored,
            tally.nodes,
            tally.dispatched,
            tally.immediate,
            tally.abstained,
            tally.discarded,
            tally.author_tokens_estimate,
            if tally.selector_calls > 0 {
                format!(
                    ", selector {} call(s) / {} input tokens",
                    tally.selector_calls, tally.selector_input_tokens
                )
            } else {
                String::new()
            }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environment(value: Option<&str>) -> LaunchEnvironment {
        let mut vars: Vec<(String, String)> = Vec::new();
        if let Some(value) = value {
            vars.push((VARIABLE.to_owned(), value.to_owned()));
        }
        LaunchEnvironment::from_pairs(vars)
    }

    #[test]
    fn off_by_default_the_setting_turns_it_on_and_the_variable_wins() {
        let temp = tempfile::tempdir().expect("temp");
        let config = temp.path().join("config.toml");
        assert!(!enabled(&config, &environment(None)));
        set(&config, &environment(None), Some(true)).expect("saved");
        assert!(enabled(&config, &environment(None)));
        assert!(!enabled(&config, &environment(Some("off"))));
        set(&config, &environment(None), Some(false)).expect("saved");
        assert!(enabled(&config, &environment(Some("on"))));
        assert!(status(&config, &environment(Some("on"))).contains("overrides"));
        assert!(!enabled(&config, &environment(Some("maybe"))));
    }

    #[test]
    fn the_report_names_the_steps_saved() {
        let tally = harness_tools::BankTally {
            authored: 1,
            nodes: 3,
            dispatched: 2,
            ..harness_tools::BankTally::default()
        };
        let report = report(true, &tally);
        assert_eq!(report["model_steps_saved"], 2);
        assert_eq!(
            super::report(false, &tally),
            serde_json::json!({"enabled": false})
        );
        assert!(
            line(&tally)
                .expect("line")
                .contains("2 run without a model step")
        );
        assert!(line(&harness_tools::BankTally::default()).is_none());
    }
}
