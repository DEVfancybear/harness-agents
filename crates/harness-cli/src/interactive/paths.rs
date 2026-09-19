//! User-scoped path resolution for the interactive launch.
//!
//! Precedence follows the `HA_LAUNCH` plan: an explicit option, then `HA_HOME`, then
//! the platform default. The environment and the platform are injected so both
//! rule sets are testable on one machine and no test can read or mutate the real
//! user environment.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessError};

/// Platform whose path rules apply to this launch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPlatform {
    Windows,
    Linux,
}

impl HostPlatform {
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Environment snapshot used for path resolution.
///
/// Tests build it from pairs; only the binary calls the real capture.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LaunchEnvironment {
    vars: BTreeMap<OsString, OsString>,
}

impl LaunchEnvironment {
    #[must_use]
    pub fn capture() -> Self {
        Self::from_pairs(std::env::vars_os())
    }

    #[must_use]
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        Self {
            vars: pairs
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        }
    }

    /// Raw value for one variable; used for presence checks only.
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&OsStr> {
        self.vars.get(OsStr::new(name)).map(OsString::as_os_str)
    }

    fn directory(&self, names: &[&str]) -> Option<PathBuf> {
        names.iter().find_map(|name| {
            let value = self.value(name)?;
            if value.is_empty() {
                None
            } else {
                Some(PathBuf::from(value))
            }
        })
    }
}

/// Which rule produced a resolved location.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PathOrigin {
    /// Explicit command option for this launch.
    ExplicitOption,
    /// `HA_HOME` fixture or override root.
    HaHome,
    /// Platform default under the user profile.
    PlatformDefault,
}

impl PathOrigin {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ExplicitOption => "explicit option",
            Self::HaHome => "HA_HOME",
            Self::PlatformDefault => "platform default",
        }
    }
}

/// Resolved user-scoped locations for one launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPaths {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub config_origin: PathOrigin,
    pub data_origin: PathOrigin,
}

impl ResolvedPaths {
    /// Per-project store directory under the user data root.
    #[must_use]
    pub fn project_data_dir(&self, project_key: &str) -> PathBuf {
        self.data_dir.join("projects").join(project_key)
    }
}

/// Inputs for path resolution.
#[derive(Clone, Copy, Debug)]
pub struct PathRequest<'a> {
    pub platform: HostPlatform,
    pub environment: &'a LaunchEnvironment,
    /// Explicit data directory from a command option; highest precedence.
    pub explicit_data_dir: Option<&'a Path>,
}

/// Resolve the configuration file and data directory for this launch.
pub fn resolve(request: &PathRequest<'_>) -> Result<ResolvedPaths, HarnessError> {
    let environment = request.environment;
    let ha_home = environment.directory(&["HA_HOME"]);
    let defaults = platform_defaults(request.platform, environment);

    let (config_file, config_origin) = if let Some(home) = &ha_home {
        (home.join("config.toml"), PathOrigin::HaHome)
    } else if let Some(path) = defaults.config_file {
        (path, PathOrigin::PlatformDefault)
    } else {
        return Err(unresolved(
            request.platform,
            "user configuration file",
            ErrorCode::ConfigReadError,
        ));
    };

    let (data_dir, data_origin) = if let Some(explicit) = request.explicit_data_dir {
        (explicit.to_path_buf(), PathOrigin::ExplicitOption)
    } else if let Some(home) = &ha_home {
        (home.join("data"), PathOrigin::HaHome)
    } else if let Some(path) = defaults.data_dir {
        (path, PathOrigin::PlatformDefault)
    } else {
        return Err(unresolved(
            request.platform,
            "user data directory",
            ErrorCode::StorageOpenFailed,
        ));
    };

    Ok(ResolvedPaths {
        config_file,
        data_dir,
        config_origin,
        data_origin,
    })
}

struct PlatformDefaults {
    config_file: Option<PathBuf>,
    data_dir: Option<PathBuf>,
}

fn platform_defaults(platform: HostPlatform, environment: &LaunchEnvironment) -> PlatformDefaults {
    match platform {
        HostPlatform::Windows => PlatformDefaults {
            config_file: environment
                .directory(&["APPDATA"])
                .map(|base| base.join("HarnessAgents").join("config.toml")),
            data_dir: environment
                .directory(&["LOCALAPPDATA"])
                .map(|base| base.join("HarnessAgents").join("data")),
        },
        HostPlatform::Linux => {
            let config_base = environment.directory(&["XDG_CONFIG_HOME"]).or_else(|| {
                environment
                    .directory(&["HOME"])
                    .map(|home| home.join(".config"))
            });
            let data_base = environment.directory(&["XDG_DATA_HOME"]).or_else(|| {
                environment
                    .directory(&["HOME"])
                    .map(|home| home.join(".local").join("share"))
            });
            PlatformDefaults {
                config_file: config_base
                    .map(|base| base.join("harness-agents").join("config.toml")),
                data_dir: data_base.map(|base| base.join("harness-agents")),
            }
        }
    }
}

fn unresolved(platform: HostPlatform, what: &str, code: ErrorCode) -> HarnessError {
    let variables = match platform {
        HostPlatform::Windows => "APPDATA and LOCALAPPDATA",
        HostPlatform::Linux => "XDG_CONFIG_HOME/XDG_DATA_HOME or HOME",
    };
    HarnessError::new(
        code,
        format!("cannot resolve the {what}: set HA_HOME or {variables}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{HostPlatform, LaunchEnvironment, PathOrigin, PathRequest, ResolvedPaths, resolve};
    use std::path::{Path, PathBuf};

    fn environment(pairs: &[(&str, &str)]) -> LaunchEnvironment {
        LaunchEnvironment::from_pairs(pairs.iter().map(|(name, value)| (*name, *value)))
    }

    fn resolved(pairs: &[(&str, &str)], platform: HostPlatform) -> ResolvedPaths {
        let environment = environment(pairs);
        resolve(&PathRequest {
            platform,
            environment: &environment,
            explicit_data_dir: None,
        })
        .expect("paths resolve")
    }

    #[test]
    fn h02_windows_defaults_follow_the_documented_user_locations() {
        let paths = resolved(
            &[
                ("APPDATA", "C:/Users/example/AppData/Roaming"),
                ("LOCALAPPDATA", "C:/Users/example/AppData/Local"),
                ("USERPROFILE", "C:/Users/example"),
            ],
            HostPlatform::Windows,
        );
        assert_eq!(
            paths.config_file,
            PathBuf::from("C:/Users/example/AppData/Roaming/HarnessAgents/config.toml")
        );
        assert_eq!(
            paths.data_dir,
            PathBuf::from("C:/Users/example/AppData/Local/HarnessAgents/data")
        );
        assert_eq!(paths.config_origin, PathOrigin::PlatformDefault);
        assert_eq!(paths.data_origin, PathOrigin::PlatformDefault);
    }

    #[test]
    fn h02_linux_defaults_prefer_xdg_then_home() {
        let xdg = resolved(
            &[
                ("XDG_CONFIG_HOME", "/home/example/.config-custom"),
                ("XDG_DATA_HOME", "/home/example/.data-custom"),
                ("HOME", "/home/example"),
            ],
            HostPlatform::Linux,
        );
        assert_eq!(
            xdg.config_file,
            PathBuf::from("/home/example/.config-custom/harness-agents/config.toml")
        );
        assert_eq!(
            xdg.data_dir,
            PathBuf::from("/home/example/.data-custom/harness-agents")
        );

        let home_only = resolved(&[("HOME", "/home/example")], HostPlatform::Linux);
        assert_eq!(
            home_only.config_file,
            PathBuf::from("/home/example/.config/harness-agents/config.toml")
        );
        assert_eq!(
            home_only.data_dir,
            PathBuf::from("/home/example/.local/share/harness-agents")
        );
    }

    #[test]
    fn h02_ha_home_overrides_the_platform_default_and_stays_isolated() {
        let environment = environment(&[
            ("HA_HOME", "D:/fixture/home"),
            ("APPDATA", "C:/real/AppData/Roaming"),
            ("LOCALAPPDATA", "C:/real/AppData/Local"),
        ]);
        let paths = resolve(&PathRequest {
            platform: HostPlatform::Windows,
            environment: &environment,
            explicit_data_dir: None,
        })
        .expect("HA_HOME resolves");
        assert_eq!(
            paths.config_file,
            PathBuf::from("D:/fixture/home/config.toml")
        );
        assert_eq!(paths.data_dir, PathBuf::from("D:/fixture/home/data"));
        assert_eq!(paths.config_origin, PathOrigin::HaHome);
        assert_eq!(paths.data_origin, PathOrigin::HaHome);
        assert!(
            !paths.data_dir.starts_with("C:/real"),
            "fixture state must stay out of the real user profile"
        );
    }

    #[test]
    fn h02_explicit_data_dir_outranks_ha_home() {
        let environment = environment(&[("HA_HOME", "D:/fixture/home")]);
        let explicit = Path::new("E:/explicit/data");
        let paths = resolve(&PathRequest {
            platform: HostPlatform::Windows,
            environment: &environment,
            explicit_data_dir: Some(explicit),
        })
        .expect("explicit data dir resolves");
        assert_eq!(paths.data_dir, PathBuf::from("E:/explicit/data"));
        assert_eq!(paths.data_origin, PathOrigin::ExplicitOption);
        assert_eq!(
            paths.config_file,
            PathBuf::from("D:/fixture/home/config.toml")
        );
        assert_eq!(
            paths.project_data_dir("project_0123"),
            PathBuf::from("E:/explicit/data/projects/project_0123")
        );
    }

    #[test]
    fn h02_empty_variables_are_unset_and_an_unresolvable_home_is_an_error() {
        let environment = environment(&[("HA_HOME", ""), ("APPDATA", "")]);
        let error = resolve(&PathRequest {
            platform: HostPlatform::Windows,
            environment: &environment,
            explicit_data_dir: None,
        })
        .expect_err("an empty environment cannot resolve the user config");
        let message = error.to_string();
        assert!(message.contains("configuration"), "{message}");
        assert!(message.contains("HA_HOME"), "{message}");
    }
}
