//! prime-agent's package manager (`pa-core/src/packages`): install, remove,
//! list and update `npm:`, git and local-directory package sources against
//! the settings, and resolve the session's skills, prompt templates and
//! themes from configured packages, the settings arrays and auto-discovery.
//!
//! As in prime, a package carries skills, prompts and themes only; nothing in
//! it is executed by ha. ha's layout: the user scope is the config directory
//! (prime's agent directory), the project scope is `<workspace>/.harness`
//! (prime's `.prime/agent`), and `--offline`/`HA_OFFLINE` stands for
//! `PI_OFFLINE`.

mod git;
mod manager;
mod npm;
mod process;
pub(crate) mod resolve;
pub mod resource_config;
pub mod settings;
mod source;
mod update;

#[cfg(test)]
mod tests;

pub use manager::{
    BundledSkillsDir, ConfiguredPackage, PackageManager, PackageManagerOptions, PackageUpdate,
    ProgressAction, ProgressEvent, ProgressEventKind, UserOrProject,
};
pub use resolve::{
    MetadataSource, MissingSourceAction, PathMetadata, ResolvedPaths, ResolvedResource,
    ResourceOrigin, ResourceType,
};
pub use settings::{CONFIG_DIR_NAME, SettingsManager};
pub use source::{GitSource, LocalSource, NpmSource, ParsedSource, SourceScope, parse_git_url};

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Network probe timeout for npm/git operations (10s).
pub(crate) use npm::NETWORK_TIMEOUT_MS;

static OFFLINE: AtomicBool = AtomicBool::new(false);

/// ha's offline mode for the package manager: set from `--offline` or
/// `HA_OFFLINE` by the caller (prime reads `PI_OFFLINE`).
pub fn set_offline(offline: bool) {
    OFFLINE.store(offline, Ordering::Relaxed);
}

/// True when offline mode disables all package network operations.
pub(crate) fn is_offline_mode_enabled() -> bool {
    #[cfg(test)]
    if test_support::OFFLINE_OVERRIDE.with(std::cell::Cell::get) {
        return true;
    }
    OFFLINE.load(Ordering::Relaxed)
}

/// The user's home directory (`USERPROFILE` on Windows, `HOME` elsewhere).
pub(crate) fn platform_home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(home) = test_support::HOME_OVERRIDE.with(|home| home.borrow().clone()) {
        return Some(home);
    }
    let variables: &[&str] = if cfg!(windows) {
        &["USERPROFILE", "HOME"]
    } else {
        &["HOME"]
    };
    variables
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|path| !path.as_os_str().is_empty())
}

fn home_dir() -> PathBuf {
    platform_home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// The canonical form of a path for de-duplication; an unresolvable path is
/// kept as it is (prime's `canonicalize_path`).
pub(crate) fn canonicalize_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A resolution diagnostic (prime's `ResourceDiagnostic`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceDiagnostic {
    Warning {
        message: String,
        path: Option<String>,
    },
    Error {
        message: String,
        path: Option<String>,
    },
}

/// Stable temporary directory for resolve-only package installs (the hash
/// keys on prefix+suffix so the same source always maps to one checkout).
pub(crate) fn temporary_dir(prefix: &str, suffix: Option<&str>) -> PathBuf {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(format!("{prefix}-{}", suffix.unwrap_or_default()).as_bytes());
    let digest = hasher.finalize();
    let hash: String = digest[..4].iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    });
    std::env::temp_dir()
        .join("ha-packages")
        .join(prefix)
        .join(&hash)
        .join(suffix.unwrap_or_default())
}

#[cfg(test)]
pub(crate) mod test_support {
    /// Process-wide env reads and writes (HOME) serialize through one lock
    /// across the packages test modules: parallel test threads in the same
    /// binary otherwise race the process env.
    pub(crate) static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    thread_local! {
        /// The home directory a test sees, in place of mutating `HOME`
        /// (prime's tests set the process environment, which ha forbids).
        pub(crate) static HOME_OVERRIDE: std::cell::RefCell<Option<std::path::PathBuf>> =
            const { std::cell::RefCell::new(None) };
        /// Offline mode for one test thread (prime's tests set `PI_OFFLINE`).
        pub(crate) static OFFLINE_OVERRIDE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(crate) fn set_home(home: Option<std::path::PathBuf>) {
        HOME_OVERRIDE.with(|slot| *slot.borrow_mut() = home);
    }

    pub(crate) fn set_offline(offline: bool) {
        OFFLINE_OVERRIDE.with(|slot| slot.set(offline));
    }
}
