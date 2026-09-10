use std::fmt::Write;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{ErrorCode, HarnessError};

/// A SHA-256 digest in the sole P0 external spelling: `sha256:<lowercase hex>`.
#[derive(Clone, Debug, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn parse(value: impl Into<String>) -> Result<Self, HarnessError> {
        let value = value.into();
        let hexadecimal = value.strip_prefix("sha256:").ok_or_else(|| {
            HarnessError::new(ErrorCode::InvalidHash, "hash must start with sha256:")
        })?;
        if hexadecimal.len() != 64 || !hexadecimal.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "hash must contain exactly 64 hexadecimal characters",
            ));
        }
        if hexadecimal.bytes().any(|byte| byte.is_ascii_uppercase()) {
            return Err(HarnessError::new(
                ErrorCode::InvalidHash,
                "hash must use lowercase hexadecimal",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let mut rendered = String::from("sha256:");
        for byte in digest {
            let _ = write!(rendered, "{byte:02x}");
        }
        Self(rendered)
    }

    pub fn from_canonical_json(value: &Value) -> Result<Self, HarnessError> {
        Ok(Self::from_bytes(&canonical_json_bytes(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Render a restricted, deterministic JSON representation for hash inputs.
///
/// `harness-json-v1` intentionally excludes floating point and exponent
/// numbers until a separately reviewed format revision defines their rules.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, HarnessError> {
    let mut rendered = String::new();
    write_value(value, &mut rendered)?;
    Ok(rendered.into_bytes())
}

fn write_value(value: &Value, output: &mut String) -> Result<(), HarnessError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::String(value) => {
            let escaped = serde_json::to_string(value).map_err(|_| {
                HarnessError::new(ErrorCode::InvalidPayload, "string cannot be canonicalized")
            })?;
            output.push_str(&escaped);
        }
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                let _ = write!(output, "{integer}");
            } else if let Some(integer) = value.as_u64() {
                let _ = write!(output, "{integer}");
            } else {
                return Err(HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "harness-json-v1 does not accept floating-point numbers",
                ));
            }
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_value(value, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            output.push('{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                let escaped_key = serde_json::to_string(key).map_err(|_| {
                    HarnessError::new(ErrorCode::InvalidPayload, "key cannot be canonicalized")
                })?;
                output.push_str(&escaped_key);
                output.push(':');
                write_value(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}
