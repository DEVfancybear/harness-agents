//! Non-secret configuration loading for the interactive launch.
//!
//! The strict P0 configuration contract is unchanged: it carries no provider or
//! credential setting, so loading it can never activate runtime work. A missing
//! file is a first run rather than an error, and a corrupt file is reported with
//! its location instead of being silently replaced by defaults.

use std::path::{Path, PathBuf};

use harness_types::{ErrorCode, HarnessConfig, HarnessError};

/// Configuration state visible to the app before the first request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigState {
    /// No file yet: defaults apply and the app opens in setup state.
    FirstRun { path: PathBuf },
    /// Parsed, validated, and still free of provider or credential settings.
    Loaded {
        path: PathBuf,
        config: HarnessConfig,
    },
}

impl ConfigState {
    #[must_use]
    pub const fn is_first_run(&self) -> bool {
        matches!(self, Self::FirstRun { .. })
    }

    /// One-line description for the launch header.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::FirstRun { path } => format!("first run, defaults ({})", path.display()),
            Self::Loaded { path, config } => {
                format!(
                    "schema_version={} ({})",
                    config.schema_version,
                    path.display()
                )
            }
        }
    }
}

/// Load the user configuration file.
pub fn load(path: &Path) -> Result<ConfigState, HarnessError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConfigState::FirstRun {
                path: path.to_path_buf(),
            });
        }
        Err(error) => {
            return Err(HarnessError::new(
                ErrorCode::ConfigReadError,
                format!(
                    "configuration file {} could not be read: {error}",
                    path.display()
                ),
            ));
        }
    };

    let config: HarnessConfig = toml::from_str(&contents)
        .map_err(|error| invalid(path, error.to_string().contains("unknown field")))?;
    if let Err(error) = config.validate() {
        return Err(HarnessError::new(
            error.code(),
            format!("configuration file {} is invalid: {error}", path.display()),
        ));
    }
    Ok(ConfigState::Loaded {
        path: path.to_path_buf(),
        config,
    })
}

/// A corrupt file is reported with its location and a safe next step.
///
/// The raw parser message is deliberately not echoed: a TOML type error can
/// quote the value it rejected, and a value in this file could be a secret a
/// user placed in the wrong file. The explicit validate command prints details
/// on demand instead.
fn invalid(path: &Path, unknown_field: bool) -> HarnessError {
    let code = if unknown_field {
        ErrorCode::ConfigUnknownField
    } else {
        ErrorCode::ConfigParseError
    };
    let detail = if unknown_field {
        "it has a key this schema does not allow"
    } else {
        "it is not valid TOML for this schema version"
    };
    HarnessError::new(
        code,
        format!(
            "configuration file {} is invalid: {detail}; run ha config validate --config {} for details",
            path.display(),
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{ConfigState, load};
    use harness_types::ErrorCode;
    use std::path::PathBuf;

    fn fixture(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    #[test]
    fn h02_missing_configuration_is_a_first_run_not_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("config.toml");
        let state = load(&path).expect("a missing file is a first run");
        assert!(state.is_first_run());
        assert!(state.describe().contains(&path.display().to_string()));
        assert!(state.describe().contains("first run"));
    }

    #[test]
    fn h02_valid_configuration_loads_and_is_described_with_its_schema_version() {
        let state = load(&fixture("tests/fixtures/p0/config/valid.toml")).expect("valid config");
        let ConfigState::Loaded { config, .. } = &state else {
            panic!("a valid file must load, got {state:?}");
        };
        assert_eq!(config.schema_version, 1);
        assert!(state.describe().contains("schema_version=1"));
    }

    #[test]
    fn h02_unknown_field_and_unsupported_schema_keep_their_codes_and_name_the_path() {
        let unknown = load(&fixture("tests/fixtures/p0/config/unknown-field.toml"))
            .expect_err("an unknown field is rejected");
        assert_eq!(unknown.code(), ErrorCode::ConfigUnknownField);
        assert!(unknown.to_string().contains("unknown-field.toml"));

        let unsupported = load(&fixture("tests/fixtures/p0/config/unsupported-schema.toml"))
            .expect_err("an unsupported schema version is rejected");
        assert_eq!(unsupported.code(), ErrorCode::UnsupportedSchemaVersion);
        assert!(unsupported.to_string().contains("unsupported-schema.toml"));
    }

    #[test]
    fn h02_corrupt_configuration_is_actionable_and_never_replaced_by_defaults() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("config.toml");
        std::fs::write(&path, "schema_version = \"not-a-number\"\n").expect("fixture write");
        let error = load(&path).expect_err("corrupt config is an error");
        assert_eq!(error.code(), ErrorCode::ConfigParseError);
        let message = error.to_string();
        assert!(message.contains("config.toml"), "{message}");
        assert!(message.contains("ha config validate"), "{message}");
        assert!(
            !message.contains("not-a-number"),
            "a rejected value must not be echoed back: {message}"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("file survives"),
            "schema_version = \"not-a-number\"\n",
            "a corrupt file is never overwritten"
        );
    }
}
