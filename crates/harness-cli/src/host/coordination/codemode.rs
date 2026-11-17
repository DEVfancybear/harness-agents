//! The codemode switch, on by default.
//!
//! On, the model is offered `codemode`: one JavaScript program that calls
//! the turn's tools as `await tools.<name>(args)`, each call gated as the
//! model's own, and only what the program prints and returns comes back
//! (`harness_tools::codemode`, `docs/architecture/CODEMODE.en.md`). It adds
//! a tool and its declarations to every request; turn it off with
//! `/codemode off`, settings.json `"codemode": false`, or `HA_CODEMODE=off`,
//! which wins over the setting either way. Children run with their parent's
//! setting.

use std::path::Path;

use super::paths::LaunchEnvironment;

/// The settings.json key.
pub const SETTING: &str = "codemode";
/// The environment switch: `on` or `off` (also `1`/`0`, `true`/`false`,
/// `yes`/`no`); it overrides the setting.
pub const VARIABLE: &str = "HA_CODEMODE";

fn parse(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "on" | "1" | "true" | "yes" => Some(true),
        "off" | "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn forced(environment: &LaunchEnvironment) -> Option<bool> {
    environment
        .value(VARIABLE)
        .and_then(|value| value.to_str())
        .and_then(parse)
}

fn setting(config_file: &Path) -> Option<bool> {
    super::config::load_setting(config_file, SETTING).and_then(|setting| setting.as_bool())
}

/// Whether codemode is on: `HA_CODEMODE` when it says, else the setting,
/// else on.
#[must_use]
pub fn enabled(config_file: &Path, environment: &LaunchEnvironment) -> bool {
    forced(environment)
        .or_else(|| setting(config_file))
        .unwrap_or(true)
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
    harness_types::diag::info("codemode", "status")
        .field("enabled", enabled(config_file, environment))
        .emit();
    Ok(status(config_file, environment))
}

/// Codemode for a driver: each conversation's `store()` kept under
/// `<data-dir>/codemode`, so a resumed conversation loads it.
#[must_use]
pub fn codemode(data_dir: &Path) -> harness_tools::Codemode {
    harness_tools::Codemode {
        store_dir: Some(data_dir.join("codemode")),
    }
}

/// `/codemode status`.
#[must_use]
pub fn status(config_file: &Path, environment: &LaunchEnvironment) -> String {
    let forced = forced(environment)
        .map(|on| {
            format!(
                " {VARIABLE}={} overrides the setting.",
                if on { "on" } else { "off" }
            )
        })
        .unwrap_or_default();
    let source = if setting(config_file).is_some() {
        format!("settings.json {SETTING}")
    } else {
        "the default".to_owned()
    };
    if enabled(config_file, environment) {
        format!(
            "Codemode: on ({source}). The model may run one JavaScript program that calls its tools, each call approved and checked as its own, and reads only what the program prints and returns. /codemode off turns it off.{forced}"
        )
    } else {
        format!(
            "Codemode: off ({source}). /codemode on offers the model one JavaScript program that calls its tools, so a step of many calls whose results it only filters or combines costs one result in the context.{forced}"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::LaunchEnvironment;
    use super::{SETTING, enabled, set, status};

    #[test]
    fn is_on_until_the_setting_or_the_variable_turns_it_off() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("settings.json");
        let plain = LaunchEnvironment::from_pairs(Vec::<(String, String)>::new());
        assert!(enabled(&config, &plain));
        assert!(status(&config, &plain).starts_with("Codemode: on (the default)"));
        set(&config, &plain, Some(false)).unwrap();
        assert!(!enabled(&config, &plain));
        assert!(status(&config, &plain).starts_with("Codemode: off (settings.json codemode)"));
        let saved = std::fs::read_to_string(&config).unwrap();
        assert!(saved.contains(SETTING), "{saved}");
        let forced_on = LaunchEnvironment::from_pairs([("HA_CODEMODE", "on")]);
        assert!(enabled(&config, &forced_on));
        assert!(status(&config, &forced_on).contains("HA_CODEMODE=on overrides"));
    }
}
