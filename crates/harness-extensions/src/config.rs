//! Config explain: which layer won, and what a change costs to apply.
//!
//! Two questions a user asks about configuration, and both have to be answerable
//! without starting anything:
//!
//! * **Where did this value come from?** Layers are ordered, and the answer
//!   names every layer that proposed the key, not just the winner.
//! * **When does a change take effect?** Most settings are re-read when the next
//!   context packet is compiled. The ones that decide which external processes
//!   exist cannot be, because a running process set is not a value. Those are
//!   reported as requiring a restart instead of being promised live.
//!
//! Explaining configuration never reads a secret value and never starts an
//! executable.

use std::collections::BTreeMap;

use harness_types::{ContentHash, ErrorCode};

use crate::contracts::{ExtensionError, ExtensionInventoryEntry};

/// Which configuration layer a value came from, lowest precedence first.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ConfigLayerKind {
    Builtin,
    User,
    TrustedProject,
    Profile,
    CliOverride,
}

impl ConfigLayerKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::User => "user",
            Self::TrustedProject => "trusted_project",
            Self::Profile => "profile",
            Self::CliOverride => "cli_override",
        }
    }

    /// Higher wins.
    #[must_use]
    pub const fn precedence(self) -> u8 {
        match self {
            Self::Builtin => 0,
            Self::User => 1,
            Self::TrustedProject => 2,
            Self::Profile => 3,
            Self::CliOverride => 4,
        }
    }
}

/// Keys whose value decides which external processes exist.
///
/// A running process set is not a configuration value that can be swapped under
/// a live turn, so a change to one of these is reported as needing a restart.
/// Anything else is re-read at the next admission boundary, when the next packet
/// is compiled.
pub const RESTART_REQUIRED_PREFIXES: &[&str] = &["extensions.", "mcp.", "plugins."];

/// One configuration layer as the host read it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigLayer {
    pub kind: ConfigLayerKind,
    /// Where this layer came from: a path, or `builtin`.
    pub origin: String,
    pub values: BTreeMap<String, String>,
}

impl ConfigLayer {
    #[must_use]
    pub fn new(kind: ConfigLayerKind, origin: impl Into<String>) -> Self {
        Self {
            kind,
            origin: origin.into(),
            values: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_value(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(key.into(), value.into());
        self
    }
}

/// One key's resolved value, with every layer that proposed it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveEntry {
    pub key: String,
    pub value: String,
    pub winner: ConfigLayerKind,
    pub winner_origin: String,
    /// Layers that proposed a different value, lowest precedence first.
    pub overridden: Vec<(ConfigLayerKind, String)>,
}

impl EffectiveEntry {
    /// Whether more than one layer proposed this key.
    #[must_use]
    pub fn is_contested(&self) -> bool {
        !self.overridden.is_empty()
    }
}

/// When a change to one key takes effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReloadBoundary {
    /// Re-read when the next context packet is compiled. No process changes.
    NextAdmission,
    /// The set of external processes changes, which a live turn cannot absorb.
    RestartRequired,
}

impl ReloadBoundary {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NextAdmission => "next_admission",
            Self::RestartRequired => "restart_required",
        }
    }

    /// The boundary a change to one key lands on.
    #[must_use]
    pub fn for_key(key: &str) -> Self {
        if RESTART_REQUIRED_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
        {
            Self::RestartRequired
        } else {
            Self::NextAdmission
        }
    }
}

/// One known extension that is not contributing, and why.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InactivePluginView {
    pub plugin_id: String,
    pub state: String,
    pub reason: Option<String>,
    pub generation: u64,
}

/// The full answer to "what config is in force, and what is not running".
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigExplain {
    pub effective: Vec<EffectiveEntry>,
    pub inactive: Vec<InactivePluginView>,
    pub active_plugins: Vec<String>,
    /// The strongest boundary any changed key would need. Present so a caller
    /// does not have to scan the entries to learn whether a restart is implied.
    pub reload_boundary: ReloadBoundary,
    pub config_digest: ContentHash,
}

impl ConfigExplain {
    #[must_use]
    pub fn entry(&self, key: &str) -> Option<&EffectiveEntry> {
        self.effective.iter().find(|entry| entry.key == key)
    }

    /// Keys whose change would need a restart.
    #[must_use]
    pub fn restart_sensitive_keys(&self) -> Vec<String> {
        self.effective
            .iter()
            .filter(|entry| ReloadBoundary::for_key(&entry.key) == ReloadBoundary::RestartRequired)
            .map(|entry| entry.key.clone())
            .collect()
    }
}

/// Resolve configuration layers and report what is running.
///
/// Layers are applied in precedence order, so the last writer of a key wins and
/// every earlier proposal is retained as an override. Values are returned as
/// they were configured: a secret reference stays a reference, because resolving
/// one is a separate, explicitly granted act.
pub fn explain_config(
    layers: &[ConfigLayer],
    inventory: &[ExtensionInventoryEntry],
) -> Result<ConfigExplain, ExtensionError> {
    let mut ordered = layers.to_vec();
    ordered.sort_by_key(|layer| layer.kind.precedence());

    let mut winners: BTreeMap<String, (String, ConfigLayerKind, String)> = BTreeMap::new();
    let mut overridden: BTreeMap<String, Vec<(ConfigLayerKind, String)>> = BTreeMap::new();
    for layer in &ordered {
        for (key, value) in &layer.values {
            if key.trim().is_empty() {
                return Err(ExtensionError::new(
                    ErrorCode::ConfigParseError,
                    "a configuration key must not be empty",
                ));
            }
            match winners.get(key) {
                Some((existing, existing_kind, existing_origin)) => {
                    if existing != value {
                        overridden
                            .entry(key.clone())
                            .or_default()
                            .push((*existing_kind, format!("{existing} ({existing_origin})")));
                    }
                    winners.insert(
                        key.clone(),
                        (value.clone(), layer.kind, layer.origin.clone()),
                    );
                }
                None => {
                    winners.insert(
                        key.clone(),
                        (value.clone(), layer.kind, layer.origin.clone()),
                    );
                }
            }
        }
    }

    let mut effective = winners
        .into_iter()
        .map(|(key, (value, winner, winner_origin))| EffectiveEntry {
            overridden: overridden.remove(&key).unwrap_or_default(),
            key,
            value,
            winner,
            winner_origin,
        })
        .collect::<Vec<_>>();
    effective.sort_by(|left, right| left.key.cmp(&right.key));

    let mut inactive = Vec::new();
    let mut active_plugins = Vec::new();
    for entry in inventory {
        match entry.inactive_reason {
            Some(reason) => inactive.push(InactivePluginView {
                plugin_id: entry.plugin_id.clone(),
                state: entry.state.to_owned(),
                reason: Some(reason.as_str().to_owned()),
                generation: entry.generation,
            }),
            None if entry.state == "active" => active_plugins.push(entry.plugin_id.clone()),
            None => inactive.push(InactivePluginView {
                plugin_id: entry.plugin_id.clone(),
                state: entry.state.to_owned(),
                reason: None,
                generation: entry.generation,
            }),
        }
    }
    inactive.sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    active_plugins.sort();

    let reload_boundary = if effective
        .iter()
        .any(|entry| ReloadBoundary::for_key(&entry.key) == ReloadBoundary::RestartRequired)
    {
        ReloadBoundary::RestartRequired
    } else {
        ReloadBoundary::NextAdmission
    };

    let config_digest = ContentHash::from_canonical_json(&serde_json::json!({
        "domain": "config-explain.v1",
        "effective": effective
            .iter()
            .map(|entry| serde_json::json!({
                "key": entry.key,
                "value": entry.value,
                "winner": entry.winner.as_str(),
                "origin": entry.winner_origin,
            }))
            .collect::<Vec<_>>(),
    }))
    .map_err(|error| ExtensionError::new(error.code(), error.to_string()))?;

    Ok(ConfigExplain {
        effective,
        inactive,
        active_plugins,
        reload_boundary,
        config_digest,
    })
}
