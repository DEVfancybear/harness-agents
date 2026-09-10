use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable machine-readable error categories used by P0 contracts and the CLI.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidHash,
    InvalidId,
    InvalidPayload,
    InvalidSequence,
    MissingAuthority,
    UnsupportedSchemaVersion,
    ConfigReadError,
    ConfigParseError,
    ConfigUnknownField,
    FixtureIntegrity,
    GateConfigurationError,
    GateRequiredTestIgnored,
    GateTestDiscoveryEmpty,
}

impl ErrorCode {
    /// The stable serialized spelling for diagnostics and scripting.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHash => "invalid_hash",
            Self::InvalidId => "invalid_id",
            Self::InvalidPayload => "invalid_payload",
            Self::InvalidSequence => "invalid_sequence",
            Self::MissingAuthority => "missing_authority",
            Self::UnsupportedSchemaVersion => "unsupported_schema_version",
            Self::ConfigReadError => "config_read_error",
            Self::ConfigParseError => "config_parse_error",
            Self::ConfigUnknownField => "config_unknown_field",
            Self::FixtureIntegrity => "fixture_integrity",
            Self::GateConfigurationError => "gate_configuration_error",
            Self::GateRequiredTestIgnored => "gate_required_test_ignored",
            Self::GateTestDiscoveryEmpty => "gate_test_discovery_empty",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An error whose code can be used without parsing prose.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{code}: {message}")]
pub struct HarnessError {
    code: ErrorCode,
    message: String,
}

impl HarnessError {
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        self.code
    }
}
