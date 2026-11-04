//! One switch for every memory feature, so memory on can be measured against
//! memory off (`docs/guides/OPERATOR_GUIDE.en.md`, "Memory on and off").
//!
//! `HA_MEMORY=off` turns off everything that carries memory from one
//! conversation to the next or turns it into prompt text; `HA_MEMORY_OFF`
//! names the features to turn off one by one, for an ablation. The
//! `memory` key of `settings.json` says the same (`{"enabled": false}`,
//! `{"off": ["digest"]}`), and the environment wins over it. The gates ask
//! this module, and nothing else decides: a feature that is off is neither
//! read into a request nor written. The conversation's own task state and
//! the project's AGENTS.md are the task and its instructions, not memory,
//! and stay.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

use super::paths::LaunchEnvironment;

/// `HA_MEMORY=off` turns every memory feature off; `on` turns them on over
/// the setting.
pub const VARIABLE: &str = "HA_MEMORY";
/// `HA_MEMORY_OFF=digest,markdown`: the features turned off, one by one.
pub const OFF_VARIABLE: &str = "HA_MEMORY_OFF";
/// The `settings.json` key: `{"enabled": false}` or `{"off": [...]}`.
pub const SETTING: &str = "memory";

/// One memory feature, as `HA_MEMORY_OFF` names it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Feature {
    /// The memory digest in every turn, the `memory` tool (and the kernel's
    /// `rlm.harness`), a child's digest and the findings it hands back.
    Digest,
    /// A project's markdown memory store: its rules and its index in a turn.
    Markdown,
    /// Session logs: writing them, and the log a new conversation is told
    /// of, `/recall`.
    SessionLog,
    /// The learning loop's results in a turn: learned rules and the notes
    /// they add to tool descriptions, the learned skill recalled for a
    /// request and the learned skills' note, and the learned skills' use
    /// counted.
    Lessons,
    /// Every background pass: the automatic review (after a turn, when a
    /// session goes quiet, after a compaction), consolidation (Deep Dream),
    /// curation, evolution, the self-evaluation, `/refine`, and what a
    /// review tells the next turn.
    Reviews,
    /// What a new conversation is told of earlier ones: the task state an
    /// earlier conversation left, and the previous conversation's last turns.
    EarlierState,
    /// The knowledge library: its catalog in a turn and the `knowledge` tool.
    Knowledge,
}

impl Feature {
    /// Every feature, in the order they are listed.
    pub const ALL: [Self; 7] = [
        Self::Digest,
        Self::Markdown,
        Self::SessionLog,
        Self::Lessons,
        Self::Reviews,
        Self::EarlierState,
        Self::Knowledge,
    ];

    /// The name `HA_MEMORY_OFF` and the setting take.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Digest => "digest",
            Self::Markdown => "markdown",
            Self::SessionLog => "session_log",
            Self::Lessons => "lessons",
            Self::Reviews => "reviews",
            Self::EarlierState => "earlier_state",
            Self::Knowledge => "knowledge",
        }
    }

    /// The feature `name` names; `-` is read as `_`, case aside.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase().replace('-', "_");
        Self::ALL.into_iter().find(|feature| feature.name() == name)
    }
}

/// Which memory features are on, and what said so.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Switch {
    off: BTreeSet<Feature>,
    /// What turned features off: `HA_MEMORY`, `HA_MEMORY_OFF` or the setting.
    source: Option<&'static str>,
    /// Names given that are no feature: said, never guessed at.
    unknown: Vec<String>,
}

impl Switch {
    /// The switch as the environment and `settings.json` beside
    /// `config_file` set it.
    #[must_use]
    pub fn load(config_file: &Path, environment: &LaunchEnvironment) -> Self {
        let text = |name: &str| {
            environment
                .value(name)
                .map(|value| value.to_string_lossy().into_owned())
        };
        Self::from_values(
            text(VARIABLE).as_deref(),
            text(OFF_VARIABLE).as_deref(),
            super::config::load_setting(config_file, SETTING).as_ref(),
        )
    }

    /// The switch the process's own environment and `settings` set: for a
    /// host request that has no launch environment of its own (the kernel's).
    #[must_use]
    pub fn of_process(settings: &Path) -> Self {
        Self::load(settings, &LaunchEnvironment::capture())
    }

    /// The switch `HA_MEMORY`, `HA_MEMORY_OFF` and the `memory` setting
    /// give. `HA_MEMORY` decides whether memory is on over the setting's
    /// `enabled`; while it is on, `HA_MEMORY_OFF` (even empty) decides which
    /// features are off over the setting's `off`.
    #[must_use]
    pub fn from_values(
        variable: Option<&str>,
        off_variable: Option<&str>,
        setting: Option<&serde_json::Value>,
    ) -> Self {
        let mut switch = Self::default();
        let enabled = match variable.map(|value| value.trim().to_ascii_lowercase()) {
            Some(value) if matches!(value.as_str(), "off" | "0" | "false" | "no") => {
                Some((false, VARIABLE))
            }
            Some(value) if matches!(value.as_str(), "on" | "1" | "true" | "yes") => {
                Some((true, VARIABLE))
            }
            Some(value) if !value.is_empty() => {
                switch.unknown.push(format!("{VARIABLE}={value}"));
                None
            }
            _ => None,
        }
        .or_else(|| {
            setting
                .and_then(|setting| setting.get("enabled"))
                .and_then(serde_json::Value::as_bool)
                .map(|enabled| (enabled, "settings.json memory"))
        });
        if let Some((false, source)) = enabled {
            switch.off = Feature::ALL.into_iter().collect();
            switch.source = Some(source);
            return switch;
        }
        let (names, source): (Vec<String>, &'static str) = match off_variable {
            Some(value) => (
                value
                    .split([',', ';', ' '])
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                OFF_VARIABLE,
            ),
            None => (
                setting
                    .and_then(|setting| setting.get("off"))
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                "settings.json memory",
            ),
        };
        for name in names.iter().filter(|name| !name.trim().is_empty()) {
            match Feature::parse(name) {
                Some(feature) => {
                    switch.off.insert(feature);
                }
                None => switch.unknown.push(name.trim().to_owned()),
            }
        }
        if !switch.off.is_empty() {
            switch.source = Some(source);
        }
        switch
    }

    /// Whether `feature` is on.
    #[must_use]
    pub fn on(&self, feature: Feature) -> bool {
        !self.off.contains(&feature)
    }

    /// Whether every feature is off.
    #[must_use]
    pub fn all_off(&self) -> bool {
        self.off.len() == Feature::ALL.len()
    }

    /// The names of the features that are off, in their order.
    #[must_use]
    pub fn off_names(&self) -> Vec<&'static str> {
        self.off.iter().map(|feature| feature.name()).collect()
    }

    /// What turned features off, when anything did.
    #[must_use]
    pub const fn source(&self) -> Option<&'static str> {
        self.source
    }

    /// One line for `/context` and `ha doctor`: `on`, or what is off and
    /// what said so, with any name that is no feature.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut text = if self.off.is_empty() {
            "on".to_owned()
        } else if self.all_off() {
            format!(
                "off ({}): {}",
                self.source.unwrap_or(VARIABLE),
                self.off_names().join(", ")
            )
        } else {
            format!(
                "on except {} ({})",
                self.off_names().join(", "),
                self.source.unwrap_or(OFF_VARIABLE)
            )
        };
        if !self.unknown.is_empty() {
            let _ = write!(
                text,
                "; not a memory feature, ignored: {} (the features: {})",
                self.unknown.join(", "),
                Feature::ALL.map(Feature::name).join(", ")
            );
        }
        text
    }

    /// The `memory` `switch` diagnostics record: what is off, and what said so.
    pub fn record(&self, task: Option<&str>) {
        let mut record = harness_types::diag::info("memory", "switch")
            .field("off", self.off_names().join(","))
            .field("all_off", self.all_off())
            .field("source", self.source().unwrap_or_default());
        if !self.unknown.is_empty() {
            record = record.field("unknown", self.unknown.join(","));
        }
        if let Some(task) = task {
            record = record.task(task);
        }
        record.emit();
    }

    /// [`Self::record`] for `task`'s turn, once per conversation and setting:
    /// a turn of the app reads the switch each time, and a record per turn
    /// would say the same thing again and again.
    pub fn record_once(&self, task: &str) {
        static SAID: std::sync::Mutex<Option<std::collections::HashMap<String, String>>> =
            std::sync::Mutex::new(None);
        let summary = self.summary();
        let new = SAID.lock().map_or(true, |mut said| {
            said.get_or_insert_with(std::collections::HashMap::new)
                .insert(task.to_owned(), summary.clone())
                .as_ref()
                != Some(&summary)
        });
        if new {
            self.record(Some(task));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Feature, Switch};

    #[test]
    fn memory_is_on_unless_something_says_off() {
        let switch = Switch::from_values(None, None, None);
        assert!(Feature::ALL.into_iter().all(|feature| switch.on(feature)));
        assert_eq!(switch.summary(), "on");
        assert!(switch.source().is_none());
    }

    #[test]
    fn ha_memory_off_turns_every_feature_off() {
        for value in ["off", "OFF", "0", "false", "no"] {
            let switch = Switch::from_values(Some(value), None, None);
            assert!(switch.all_off(), "{value}");
            assert_eq!(switch.source(), Some("HA_MEMORY"));
        }
        let switch = Switch::from_values(Some("off"), Some(""), None);
        assert!(switch.all_off(), "HA_MEMORY=off wins over an empty list");
        assert!(
            switch
                .summary()
                .starts_with("off (HA_MEMORY): digest, markdown")
        );
    }

    #[test]
    fn features_are_turned_off_by_name_alone() {
        let switch = Switch::from_values(None, Some("digest, session-log;knowledge"), None);
        assert!(!switch.on(Feature::Digest));
        assert!(!switch.on(Feature::SessionLog));
        assert!(!switch.on(Feature::Knowledge));
        assert!(switch.on(Feature::Markdown));
        assert!(switch.on(Feature::Reviews));
        assert_eq!(switch.off_names(), ["digest", "session_log", "knowledge"]);
        assert_eq!(
            switch.summary(),
            "on except digest, session_log, knowledge (HA_MEMORY_OFF)"
        );
    }

    #[test]
    fn a_name_that_is_no_feature_is_said_and_ignored() {
        let switch = Switch::from_values(None, Some("digest,dreams"), None);
        assert_eq!(switch.off_names(), ["digest"]);
        assert!(
            switch
                .summary()
                .contains("not a memory feature, ignored: dreams")
        );
        let switch = Switch::from_values(Some("sometimes"), None, None);
        assert!(!switch.all_off());
        assert!(switch.summary().contains("HA_MEMORY=sometimes"));
    }

    #[test]
    fn the_setting_says_the_same_and_the_environment_wins() {
        let disabled = serde_json::json!({"enabled": false});
        assert!(Switch::from_values(None, None, Some(&disabled)).all_off());
        assert_eq!(
            Switch::from_values(None, None, Some(&disabled)).source(),
            Some("settings.json memory")
        );
        let switch = Switch::from_values(Some("on"), None, Some(&disabled));
        assert!(!switch.all_off(), "HA_MEMORY=on wins over enabled: false");
        let some = serde_json::json!({"off": ["lessons", "reviews"]});
        let switch = Switch::from_values(None, None, Some(&some));
        assert_eq!(switch.off_names(), ["lessons", "reviews"]);
        let switch = Switch::from_values(None, Some("markdown"), Some(&some));
        assert_eq!(switch.off_names(), ["markdown"], "HA_MEMORY_OFF wins");
        let switch = Switch::from_values(None, Some(""), Some(&some));
        assert_eq!(
            switch.summary(),
            "on",
            "an empty HA_MEMORY_OFF turns none off"
        );
    }

    #[test]
    fn the_settings_file_and_the_environment_are_read() {
        let dir = tempfile::tempdir().expect("dir");
        let config = dir.path().join("config.toml");
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"memory": {"off": ["knowledge"]}}"#,
        )
        .expect("settings");
        let none = super::LaunchEnvironment::from_pairs(Vec::<(String, String)>::new());
        assert_eq!(Switch::load(&config, &none).off_names(), ["knowledge"]);
        let off = super::LaunchEnvironment::from_pairs([("HA_MEMORY", "off")]);
        assert!(Switch::load(&config, &off).all_off());
    }
}
