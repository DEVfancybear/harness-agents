//! Bounded artifact export: the bytes, the digest, and where they may land.
//!
//! Two properties matter here, and both are asserted rather than described:
//!
//! * **Integrity.** Exported bytes are read back through the store's own record
//!   and their digest is recomputed and compared with the digest the record
//!   carries. An artifact whose bytes and digest disagree is refused; it is never
//!   copied "because the row said so".
//! * **Containment of the destination.** The destination is resolved by name and
//!   refused if any component would leave the export root, so a stored or
//!   operator-supplied name cannot become a path outside it.
//!
//! Export is a host operation on its own files; it is not a sandbox boundary and
//! nothing here claims to be one.

use std::path::{Path, PathBuf};

use harness_store_sqlite::{SqliteStore, StoreError};
use harness_types::{ContentHash, ErrorCode, HarnessError};
use serde::{Deserialize, Serialize};

use super::plan::resolve_within;

/// Version of the export record shape.
pub const ARTIFACT_EXPORT_SCHEMA_VERSION: u16 = 1;

/// The largest artifact this export will copy.
///
/// The cap is deliberate: a capture is bounded by the spool quota, and an export
/// that quietly copied something far larger would be moving bytes nobody
/// budgeted for.
pub const MAX_EXPORT_BYTES: u64 = 64 * 1024 * 1024;

/// One exported artifact, with what a reader needs to check it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactExport {
    pub schema_version: u16,
    pub artifact_id: String,
    /// The digest the store recorded when it published the artifact.
    pub recorded_digest: String,
    /// The digest recomputed from the bytes that were actually written.
    pub exported_digest: String,
    pub byte_len: u64,
    /// The path, relative to the export root, the bytes were written to.
    pub relative_path: String,
    /// What the execution this artifact came from was granted, when the caller
    /// has a plan for it.
    pub provenance: Option<ExportProvenance>,
}

/// What an export records about the execution behind the artifact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExportProvenance {
    pub backend: String,
    pub backend_version: String,
    pub profile: String,
    pub enforced: Vec<String>,
    pub not_claimed: Vec<String>,
    pub lease_id: Option<String>,
}

/// Export a published artifact into `destination_root`.
///
/// The bytes are read through the store (a paged read, bounded by `page_bytes`
/// per call and by the record's own length), the digest is recomputed, and only
/// then is anything written. The written name is `<artifact_id>.bin`, resolved
/// through [`resolve_within`].
#[allow(clippy::too_many_lines)] // one export, told in order: read, verify, write, verify
pub async fn export_artifact(
    store: &SqliteStore,
    artifact_id: &str,
    destination_root: &Path,
    provenance: Option<ExportProvenance>,
    page_bytes: usize,
) -> Result<ArtifactExport, HarnessError> {
    if page_bytes == 0 {
        return Err(HarnessError::new(
            ErrorCode::InvalidPayload,
            "an export page size must be positive",
        ));
    }
    // The stored id is the only name this function accepts: a caller cannot hand
    // it a path, and the id itself is checked before it becomes one.
    let parsed = harness_types::ArtifactId::parse(artifact_id.to_owned())?;
    let first = store
        .read_artifact_page(parsed.as_str(), 0, page_bytes)
        .await
        .map_err(StoreError::into_harness_error)?
        .ok_or_else(|| {
            HarnessError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("artifact {} has no record", parsed.as_str()),
            )
        })?;
    let recorded_digest = first.content_hash.clone();
    let total = first.total_bytes;
    let mut bytes = first.bytes;
    let mut offset = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    while offset < total {
        if offset > MAX_EXPORT_BYTES {
            return Err(HarnessError::new(
                ErrorCode::OutputLimitExceeded,
                format!("artifact exceeds the {MAX_EXPORT_BYTES} byte export cap"),
            ));
        }
        let page = store
            .read_artifact_page(parsed.as_str(), offset, page_bytes)
            .await
            .map_err(StoreError::into_harness_error)?
            .ok_or_else(|| {
                HarnessError::new(
                    ErrorCode::ArtifactWriteFailed,
                    format!("artifact {} disappeared mid-export", parsed.as_str()),
                )
            })?;
        let read = u64::try_from(page.bytes.len()).unwrap_or(u64::MAX);
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&page.bytes);
        offset += read;
    }
    if offset != total {
        return Err(HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!(
                "artifact {} read back {} bytes but records {total}",
                parsed.as_str(),
                bytes.len()
            ),
        ));
    }
    let recomputed = ContentHash::from_bytes(&bytes);
    if recomputed != recorded_digest {
        return Err(HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!(
                "artifact {} bytes hash to {} but the record says {}",
                parsed.as_str(),
                recomputed.as_str(),
                recorded_digest.as_str()
            ),
        ));
    }

    let relative_path = format!("{}.bin", parsed.as_str());
    let destination = resolve_within(destination_root, &relative_path)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            HarnessError::new(
                ErrorCode::ArtifactWriteFailed,
                format!(
                    "cannot create the export directory {}: {error}",
                    parent.display()
                ),
            )
        })?;
    }
    std::fs::write(&destination, &bytes).map_err(|error| {
        HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!("cannot write {}: {error}", destination.display()),
        )
    })?;
    // Read the bytes back: the digest in the receipt is about what is on disk,
    // not about what this function intended to write.
    let written = std::fs::read(&destination).map_err(|error| {
        HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!("cannot read back {}: {error}", destination.display()),
        )
    })?;
    let exported_digest = ContentHash::from_bytes(&written);
    if exported_digest != recomputed {
        return Err(HarnessError::new(
            ErrorCode::ArtifactWriteFailed,
            format!(
                "exported bytes hash to {} but the artifact hashes to {}",
                exported_digest.as_str(),
                recomputed.as_str()
            ),
        ));
    }
    Ok(ArtifactExport {
        schema_version: ARTIFACT_EXPORT_SCHEMA_VERSION,
        artifact_id: parsed.as_str().to_owned(),
        recorded_digest: recorded_digest.as_str().to_owned(),
        exported_digest: exported_digest.as_str().to_owned(),
        byte_len: u64::try_from(written.len()).unwrap_or(u64::MAX),
        relative_path,
        provenance,
    })
}

/// Where an export would land, without writing anything.
pub fn export_destination(
    destination_root: &Path,
    artifact_id: &str,
) -> Result<PathBuf, HarnessError> {
    let parsed = harness_types::ArtifactId::parse(artifact_id.to_owned())?;
    resolve_within(destination_root, &format!("{}.bin", parsed.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_name_cannot_leave_the_export_root() {
        let root = Path::new("C:/export-root");
        for hostile in [
            "../escape.bin",
            "..\\escape.bin",
            "nested/../../escape.bin",
            "C:/windows/system32/escape.bin",
            "/etc/passwd",
            "\\\\server\\share\\escape.bin",
        ] {
            assert!(
                resolve_within(root, hostile).is_err(),
                "{hostile} must not resolve inside the root"
            );
        }
        assert!(resolve_within(root, "artifact_ok.bin").is_ok());
        assert!(resolve_within(root, "./artifact_ok.bin").is_ok());
        assert!(resolve_within(root, "   ").is_err());
    }
}
