//! A new release, announced at launch as Codex does ("Update available!").
//!
//! The newest version the npm registry lists for `harness-agents` is fetched on
//! a thread of its own at every launch and kept in
//! `<data dir>/cache/version.json`; the launch reads what an earlier one saved,
//! so it never waits on the network. Codex asks at most every 20 hours; measured,
//! that hid a release published the morning after a check until the next day. `HA_NO_UPDATE_CHECK=1` or
//! `"checkForUpdates": false` in `settings.json` turns it off.

use std::path::{Path, PathBuf};

use super::paths::LaunchEnvironment;

/// The npm registry's record of the newest `harness-agents` release.
pub const LATEST_URL: &str = "https://registry.npmjs.org/harness-agents/latest";

/// The version this build is.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("cache").join("version.json")
}

/// Whether the check runs: on unless `HA_NO_UPDATE_CHECK` or the setting
/// turns it off.
#[must_use]
pub fn enabled(environment: &LaunchEnvironment, config_file: &Path) -> bool {
    let off = environment
        .value("HA_NO_UPDATE_CHECK")
        .and_then(|value| value.to_str())
        .is_some_and(|value| !matches!(value.trim(), "" | "0" | "false" | "no" | "off"));
    !off && super::config::load_setting(config_file, "checkForUpdates")
        .and_then(|value| value.as_bool())
        .unwrap_or(true)
}

/// Ask the registry again, on a thread of its own: the next launch shows what
/// it saved.
pub fn refresh_in_background(data_dir: &Path) {
    let path = cache_path(data_dir);
    let _ = std::thread::Builder::new()
        .name("ha-update-check".to_owned())
        .spawn(move || {
            let Some(latest) = super::providers::download(LATEST_URL)
                .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
                .and_then(|record| record["version"].as_str().map(str::to_owned))
                .filter(|version| parse(version).is_some())
            else {
                return;
            };
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let staged = path.with_extension("json.staged");
            let text = serde_json::json!({ "latest_version": latest }).to_string();
            if std::fs::write(&staged, text).is_ok() {
                let _ = std::fs::rename(&staged, &path);
            }
        });
}

/// The line the launch shows when a newer release is out, from what an
/// earlier check saved.
#[must_use]
pub fn notice(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cache_path(data_dir)).ok()?;
    let latest = serde_json::from_str::<serde_json::Value>(&text).ok()?["latest_version"]
        .as_str()?
        .to_owned();
    is_newer(&latest, CURRENT).then(|| {
        format!(
            "✨ Update available! {CURRENT} -> {latest}. Run {} to update.",
            install_hint()
        )
    })
}

/// How this copy of `ha` is updated: npm puts it under `node_modules`.
fn install_hint() -> &'static str {
    let from_npm = std::env::current_exe().is_ok_and(|path| {
        path.components()
            .any(|part| part.as_os_str().eq_ignore_ascii_case("node_modules"))
    });
    if from_npm {
        "`npm install -g harness-agents@latest`"
    } else {
        "`npm install -g harness-agents@latest` (or `scripts/Install-Ha.ps1` in a checkout)"
    }
}

/// `major.minor.patch` and a prerelease tag, as semver orders them.
fn parse(version: &str) -> Option<(u64, u64, u64, Option<&str>)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split('+').next()?;
    let (base, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(base, pre)| (base, Some(pre)));
    let mut numbers = base.split('.');
    let parsed = (
        numbers.next()?.parse().ok()?,
        numbers.next()?.parse().ok()?,
        numbers.next()?.parse().ok()?,
        prerelease,
    );
    numbers.next().is_none().then_some(parsed)
}

/// Whether `candidate` is a strictly newer release than `current`; a
/// prerelease is never offered over a release of the same numbers.
#[must_use]
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some(candidate), Some(current)) = (parse(candidate), parse(current)) else {
        return false;
    };
    let numbers = |version: (u64, u64, u64, Option<&str>)| (version.0, version.1, version.2);
    match numbers(candidate).cmp(&numbers(current)) {
        std::cmp::Ordering::Greater => candidate.3.is_none(),
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => current.3.is_some() && candidate.3.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CURRENT, cache_path, enabled, is_newer, notice};
    use crate::interactive::paths::LaunchEnvironment;

    #[test]
    fn only_a_strictly_newer_release_is_offered() {
        assert!(is_newer("0.1.6", "0.1.5"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.5", "0.1.5"));
        assert!(!is_newer("0.1.4", "0.1.5"));
        assert!(!is_newer("0.2.0-beta.1", "0.1.5"), "no prerelease");
        assert!(is_newer("0.2.0", "0.2.0-beta.1"));
        assert!(!is_newer("garbage", "0.1.5"));
    }

    #[test]
    fn the_notice_reads_what_the_last_check_saved() {
        let data = tempfile::tempdir().expect("temp");
        assert_eq!(notice(data.path()), None, "nothing checked yet");
        let path = cache_path(data.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("cache");
        std::fs::write(&path, r#"{"latest_version": "999.0.0"}"#).expect("saved");
        let line = notice(data.path()).expect("a newer release");
        assert!(line.contains(&format!("{CURRENT} -> 999.0.0")), "{line}");
        assert!(
            line.contains("npm install -g harness-agents@latest"),
            "{line}"
        );
        std::fs::write(&path, format!(r#"{{"latest_version": "{CURRENT}"}}"#)).expect("saved");
        assert_eq!(notice(data.path()), None, "up to date");
    }

    #[test]
    fn the_check_can_be_turned_off() {
        let home = tempfile::tempdir().expect("temp");
        let config = home.path().join("config.toml");
        let none = LaunchEnvironment::from_pairs::<_, &str, &str>([]);
        assert!(enabled(&none, &config));
        assert!(!enabled(
            &LaunchEnvironment::from_pairs([("HA_NO_UPDATE_CHECK", "1")]),
            &config
        ));
        std::fs::write(
            home.path().join("settings.json"),
            r#"{"checkForUpdates": false}"#,
        )
        .expect("settings");
        assert!(!enabled(&none, &config));
    }
}
