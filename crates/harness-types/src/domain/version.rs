//! The versioned document envelope and its compatibility policy.
//!
//! The envelope carries `schema_version`, `kind`, `critical` and an
//! object-shaped `payload`. The envelope itself is strict: an unknown top-level
//! field is a rejected document. Inside the payload, unknown *optional* fields
//! are preserved so a newer writer does not lose data through this reader. An
//! unknown *critical* kind is blocked at the point of decode: it may not feed a
//! mutation or a replay projection.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::{ContentHash, ErrorCode, HarnessError, P0_SCHEMA_VERSION, canonical_json_bytes};

/// One decoded versioned document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedDocument {
    pub schema_version: u16,
    pub kind: String,
    pub critical: bool,
    /// Unknown optional fields are kept, not dropped.
    pub payload: Map<String, Value>,
}

impl VersionedDocument {
    /// Decode and admit a document.
    ///
    /// `known_kinds` is the host's registry of kinds it can project. A document
    /// that is unknown but not critical is admitted for compatibility; an
    /// unknown critical document is refused with `UnknownCriticalEvent`.
    pub fn parse_json(input: &str, known_kinds: &BTreeSet<String>) -> Result<Self, HarnessError> {
        let value: Value = serde_json::from_str(input).map_err(|_| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "versioned document is not valid JSON",
            )
        })?;
        let object = value.as_object().ok_or_else(|| {
            HarnessError::new(
                ErrorCode::InvalidPayload,
                "versioned document must be a JSON object",
            )
        })?;
        let known_fields: BTreeSet<&str> = ["schema_version", "kind", "critical", "payload"].into();
        if let Some(unknown) = object
            .keys()
            .find(|key| !known_fields.contains(key.as_str()))
        {
            return Err(HarnessError::new(
                ErrorCode::InvalidPayload,
                format!("versioned document has an unknown envelope field: {unknown}"),
            ));
        }
        let schema_version = object
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|version| u16::try_from(version).ok())
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "versioned document is missing a numeric schema_version",
                )
            })?;
        if schema_version != P0_SCHEMA_VERSION {
            return Err(HarnessError::new(
                ErrorCode::UnsupportedSchemaVersion,
                format!(
                    "versioned document schema {schema_version} is not supported (this host reads {P0_SCHEMA_VERSION})"
                ),
            ));
        }
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| !kind.trim().is_empty())
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "versioned document kind is required",
                )
            })?
            .to_owned();
        let critical = object.get("critical").is_some_and(|value| match value {
            Value::Bool(value) => *value,
            _ => false,
        });
        if critical && !known_kinds.contains(&kind) {
            return Err(HarnessError::new(
                ErrorCode::UnknownCriticalEvent,
                format!("critical document kind {kind} is not known to this host"),
            ));
        }
        let payload = object
            .get("payload")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::InvalidPayload,
                    "versioned document payload must be an object",
                )
            })?;
        Ok(Self {
            schema_version,
            kind,
            critical,
            payload,
        })
    }

    /// Re-render the document as `harness-json-v1`: independent of input key
    /// order, and the only bytes a hash may be taken over.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, HarnessError> {
        canonical_json_bytes(&self.to_value())
    }

    pub fn content_hash(&self) -> Result<ContentHash, HarnessError> {
        ContentHash::from_canonical_json(&self.to_value())
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        Value::Object(
            [
                (
                    "schema_version".to_owned(),
                    Value::from(self.schema_version),
                ),
                ("kind".to_owned(), Value::from(self.kind.as_str())),
                ("critical".to_owned(), Value::Bool(self.critical)),
                ("payload".to_owned(), Value::Object(self.payload.clone())),
            ]
            .into_iter()
            .collect(),
        )
    }

    #[must_use]
    pub fn from_payload(
        kind: impl Into<String>,
        critical: bool,
        payload: Map<String, Value>,
    ) -> Self {
        Self {
            schema_version: P0_SCHEMA_VERSION,
            kind: kind.into(),
            critical,
            payload,
        }
    }
}

/// The document kinds this host can project. A critical document of another
/// kind is refused instead of being partially applied.
#[must_use]
pub fn known_document_kinds() -> BTreeSet<String> {
    [
        "compaction.started",
        "decision.updated",
        "input.admitted",
        "instruction.extracted",
        "model.response",
        "receipt.recorded",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::{VersionedDocument, known_document_kinds};
    use crate::ErrorCode;

    const KNOWN: &str = r#"{"schema_version":1,"kind":"input.admitted","critical":true,
        "payload":{"text":"hello","future_optional_field":{"kept":true}}}"#;

    #[test]
    fn canonical_bytes_ignore_input_key_order() {
        let known = VersionedDocument::parse_json(KNOWN, &known_document_kinds())
            .expect("a known critical document is admitted");
        let reordered = VersionedDocument::parse_json(
            r#"{"critical":true,"payload":{"future_optional_field":{"kept":true},"text":"hello"},
                "kind":"input.admitted","schema_version":1}"#,
            &known_document_kinds(),
        )
        .expect("key order is irrelevant");
        assert_eq!(known, reordered);
        assert_eq!(
            known.canonical_bytes().expect("canonical bytes"),
            reordered.canonical_bytes().expect("canonical bytes")
        );
        assert_eq!(
            known.content_hash().expect("hash"),
            reordered.content_hash().expect("hash")
        );
        let rendered = String::from_utf8(known.canonical_bytes().expect("bytes"))
            .expect("canonical JSON is UTF-8");
        assert!(
            rendered.contains("future_optional_field"),
            "unknown optional payload fields are preserved: {rendered}"
        );
    }

    #[test]
    fn unknown_critical_rejects_and_unknown_optional_is_admitted() {
        let error = VersionedDocument::parse_json(
            r#"{"schema_version":1,"kind":"future.critical","critical":true,"payload":{}}"#,
            &known_document_kinds(),
        )
        .expect_err("unknown critical must be blocked");
        assert_eq!(error.code(), ErrorCode::UnknownCriticalEvent);

        let admitted = VersionedDocument::parse_json(
            r#"{"schema_version":1,"kind":"future.additive","critical":false,"payload":{"a":1}}"#,
            &known_document_kinds(),
        )
        .expect("unknown optional is a compatibility case");
        assert_eq!(admitted.kind, "future.additive");
    }

    #[test]
    fn unsupported_version_and_strict_envelope_are_typed() {
        let version = VersionedDocument::parse_json(
            r#"{"schema_version":2,"kind":"input.admitted","payload":{}}"#,
            &known_document_kinds(),
        )
        .expect_err("another schema version is not readable here");
        assert_eq!(version.code(), ErrorCode::UnsupportedSchemaVersion);

        let envelope = VersionedDocument::parse_json(
            r#"{"schema_version":1,"kind":"input.admitted","payload":{},"extra":true}"#,
            &known_document_kinds(),
        )
        .expect_err("the envelope itself is strict");
        assert_eq!(envelope.code(), ErrorCode::InvalidPayload);

        let payload = VersionedDocument::parse_json(
            r#"{"schema_version":1,"kind":"input.admitted","payload":[]}"#,
            &known_document_kinds(),
        )
        .expect_err("payload must be an object");
        assert_eq!(payload.code(), ErrorCode::InvalidPayload);
    }
}
