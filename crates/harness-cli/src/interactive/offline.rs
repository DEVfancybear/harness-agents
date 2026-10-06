//! prime-agent's offline mode (`--offline`, `PI_OFFLINE`): no startup network
//! operations. ha's are the model catalog refresh, the logged-in providers'
//! model lists and the update check; the bundled and cached copies keep
//! serving. `ha --offline` or `HA_OFFLINE=1` turns it on.

use std::sync::atomic::{AtomicBool, Ordering};

use super::paths::LaunchEnvironment;

static FLAG: AtomicBool = AtomicBool::new(false);

/// `--offline` was given: offline for the rest of the process.
pub fn enable() {
    FLAG.store(true, Ordering::Relaxed);
}

/// Whether startup network operations are skipped: `--offline`, or a truthy
/// `HA_OFFLINE` (1/true/yes/on, as prime reads `PI_OFFLINE`).
#[must_use]
pub fn is_offline(environment: &LaunchEnvironment) -> bool {
    FLAG.load(Ordering::Relaxed)
        || environment
            .value("HA_OFFLINE")
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_truthy_ha_offline_turns_it_on() {
        assert!(is_offline(&LaunchEnvironment::from_pairs([(
            "HA_OFFLINE",
            "1"
        )])));
        assert!(is_offline(&LaunchEnvironment::from_pairs([(
            "HA_OFFLINE",
            "TRUE"
        )])));
        assert!(!is_offline(&LaunchEnvironment::from_pairs([(
            "HA_OFFLINE",
            "0"
        )])));
    }
}
