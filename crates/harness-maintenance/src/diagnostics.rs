//! A bounded, redacted support bundle.
//!
//! A support bundle exists so a human can diagnose a host they cannot reach. It
//! is therefore the one artefact that is designed to leave the machine, and the
//! rule that shapes it is: **nothing goes in that was not chosen**. The bundle is
//! assembled from a closed list of fields, every free-text value passes a
//! redaction pass, and the default never includes a transcript, a credential or
//! a raw environment.
//!
//! Redaction is deliberately two-sided. A *value* under a name that looks like a
//! secret is replaced outright, and a value that merely *contains* something
//! shaped like a credential is replaced too. The second rule matters because the
//! first one only catches a secret that is stored under an honest name, and the
//! interesting failure is a secret pasted into a message.

use std::path::{Path, PathBuf};

use harness_store_sqlite::{SqliteStore, StorePaths};
use harness_types::{ContentHash, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::contracts::{MaintenanceError, now_unix_ms};

/// The marker written in place of a value that was withheld.
pub const REDACTED: &str = "[redacted]";

/// How many characters of any free-text field the bundle will carry.
///
/// A support bundle is a summary, not a copy of the store: a field that is
/// longer than this is clipped and says so, so a bundle cannot become the
/// transport for the transcript it is supposed to exclude.
pub const MAX_FIELD_CHARS: usize = 512;

/// Upper bound on the events whose correlation ids the bundle lists.
pub const MAX_CORRELATION_REFS: usize = 32;

/// What one bundle run produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupportBundle {
    pub output_dir: String,
    pub manifest_hash: ContentHash,
    pub files: Vec<BundleFile>,
    /// Fields the redaction pass withheld, by name. Counted so a reader can see
    /// that redaction happened rather than having to trust that it did.
    pub redacted_fields: Vec<String>,
}

/// One file in the bundle, with the digest a reader can check it against.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    pub name: String,
    pub byte_len: u64,
    pub content_hash: ContentHash,
}

/// Names whose *values* are never carried, whatever they hold.
///
/// Matched case-insensitively as a substring, so `DEEPSEEK_API_KEY`,
/// `HA_PROVIDER_TOKEN` and `authorization` are all caught by one of these.
const SECRET_NAME_MARKERS: &[&str] = &[
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "token",
    "credential",
    "authorization",
    "auth",
    "private_key",
    "session_key",
    "cookie",
];

/// Shapes that are withheld even when the name looks innocent.
///
/// A pasted key in a task objective is the case this catches: the field is
/// `objective`, which is not a secret name, and the value is a credential.
const SECRET_VALUE_MARKERS: &[&str] = &[
    "sk-",
    "bearer ",
    "-----begin ",
    "ghp_",
    "xoxb-",
    "akia",
    "password=",
    "token=",
    "secret=",
    "api_key=",
];

/// Whether a field name is one whose value must not be carried.
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SECRET_NAME_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Whether a value looks like it carries a credential.
#[must_use]
pub fn looks_like_a_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    SECRET_VALUE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Clip a free-text field to the bundle's bound, saying that it was clipped.
#[must_use]
pub fn clip(value: &str) -> String {
    if value.chars().count() <= MAX_FIELD_CHARS {
        return value.to_owned();
    }
    let mut clipped = value.chars().take(MAX_FIELD_CHARS).collect::<String>();
    clipped.push_str(" [clipped]");
    clipped
}

/// Redact one named value: withhold it, or clip what is left.
///
/// Returns the value to carry and whether it was withheld, so the caller can
/// report redaction honestly instead of silently dropping a field.
#[must_use]
pub fn redact_field(name: &str, value: &str) -> (String, bool) {
    if is_secret_name(name) || looks_like_a_secret(value) {
        return (REDACTED.to_owned(), true);
    }
    (clip(value), false)
}

/// Redact a whole JSON object, one field at a time.
///
/// Nested objects are walked; arrays of strings are redacted element by element,
/// because a list of environment values is exactly where a secret hides.
#[must_use]
pub fn redact_value(name: &str, value: &Value) -> (Value, Vec<String>) {
    match value {
        Value::String(text) => {
            let (redacted, withheld) = redact_field(name, text);
            if withheld {
                (Value::String(redacted), vec![name.to_owned()])
            } else {
                (Value::String(redacted), Vec::new())
            }
        }
        Value::Array(items) => {
            let mut withheld = Vec::new();
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let (redacted, mut names) = redact_value(name, item);
                withheld.append(&mut names);
                out.push(redacted);
            }
            (Value::Array(out), withheld)
        }
        Value::Object(map) => {
            let mut withheld = Vec::new();
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                let (redacted, mut names) = redact_value(key, item);
                withheld.append(&mut names);
                out.insert(key.clone(), redacted);
            }
            (Value::Object(out), withheld)
        }
        other => (other.clone(), Vec::new()),
    }
}

/// Build a support bundle for one data directory.
///
/// The bundle is written into `output_dir`, which must not already hold one: a
/// bundle that merged two runs would have a manifest that describes neither.
#[allow(clippy::too_many_lines)] // One closed list of fields; splitting hides what is carried.
pub async fn build_support_bundle(
    data_dir: impl AsRef<Path>,
    output_dir: impl AsRef<Path>,
    environment: &[(String, String)],
    config: Option<&Value>,
) -> Result<SupportBundle, MaintenanceError> {
    let data_dir = data_dir.as_ref();
    let output_dir = output_dir.as_ref().to_path_buf();
    if output_dir.exists()
        && output_dir
            .read_dir()
            .is_ok_and(|mut entries| entries.next().is_some())
    {
        return Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            format!(
                "{} already holds files; refusing to mix two bundles",
                output_dir.display()
            ),
        ));
    }
    std::fs::create_dir_all(&output_dir)?;

    let mut redacted_fields = Vec::new();

    // Environment: names only. A support bundle that carried values would be a
    // credential export with a diagnostics label on it, and the caller's
    // environment is not the host's to publish.
    let mut environment_names = environment
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    environment_names.sort();
    for (name, value) in environment {
        if is_secret_name(name) || looks_like_a_secret(value) {
            redacted_fields.push(format!("environment.{name}"));
        }
    }

    // Config: carried through the redaction pass, because a config is where a
    // host's real settings live and the operator needs to see them.
    let (config_value, mut config_withheld) = match config {
        Some(value) => redact_value("config", value),
        None => (json!({}), Vec::new()),
    };
    redacted_fields.append(&mut config_withheld);

    let paths = StorePaths::new(data_dir);
    let store = SqliteStore::open_read_only(data_dir)
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    let diagnostics = store
        .diagnostics()
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    let revisions = store
        .all_schema_revisions()
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    let sessions = store
        .list_sessions()
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    let tasks = store
        .list_task_nodes()
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    let retention = crate::retention::retention_summary(&store)
        .await
        .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
    // Correlation references: the newest events of each session, as ids and
    // sequence numbers. Not their payloads - a payload is where the transcript
    // is, and the point of the bundle is to point at it rather than carry it.
    let mut correlation = Vec::new();
    for session in sessions.iter().take(8) {
        let summary = store
            .session_summary(&session.session_id)
            .await
            .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
        let Some(summary) = summary else { continue };
        let through = summary.next_sequence.saturating_sub(1);
        let from = through.saturating_sub(4);
        let events = store
            .load_events_after(&session.session_id, from)
            .await
            .map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;
        for event in events.iter().take(4) {
            if correlation.len() >= MAX_CORRELATION_REFS {
                break;
            }
            correlation.push(json!({
                "session_id": session.session_id,
                "event_id": event.event_id,
                "sequence": event.seq,
                "event_type": event.event_type,
                "correlation_id": event.correlation_id,
                "causation_id": event.causation_id,
            }));
        }
    }
    let store_closed = store.close().await;
    store_closed.map_err(|error| MaintenanceError::new(error.code(), error.to_string()))?;

    let host = json!({
        "schema_version": 1,
        "generated_at_unix_ms": now_unix_ms(),
        "bundle_kind": "harness-support-bundle",
        "data_dir_present": paths.database_path.is_file(),
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY,
        },
        "build": {
            "version": env!("CARGO_PKG_VERSION"),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        },
        "sqlite": {
            "foreign_keys": diagnostics.foreign_keys_enabled,
            "journal_mode": diagnostics.journal_mode,
            "synchronous": diagnostics.synchronous,
            "busy_timeout_ms": diagnostics.busy_timeout_ms,
        },
        "schema_revisions": revisions,
        "counts": {
            "sessions": sessions.len(),
            "delegated_tasks": tasks.len(),
        },
        "retention": retention,
        "environment_names": environment_names,
        "config": config_value,
        "correlation_refs": correlation,
        "excluded_by_default": [
            "credentials and environment values",
            "raw transcripts and message bodies",
            "artifact bytes",
            "provider requests and responses",
        ],
    });

    let mut files = Vec::new();
    let host_bytes = serde_json::to_vec_pretty(&host).map_err(|_| {
        MaintenanceError::new(
            ErrorCode::InvalidPayload,
            "the support bundle could not be serialized",
        )
    })?;
    files.push(write_bundle_file(&output_dir, "host.json", &host_bytes)?);

    let readme = reproducible_commands(data_dir);
    files.push(write_bundle_file(
        &output_dir,
        "REPRODUCE.md",
        readme.as_bytes(),
    )?);

    let manifest = json!({
        "schema_version": 1,
        "bundle_kind": "harness-support-bundle-manifest",
        "generated_at_unix_ms": now_unix_ms(),
        "files": files,
        "redacted_field_count": redacted_fields.len(),
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|_| {
        MaintenanceError::new(
            ErrorCode::InvalidPayload,
            "the bundle manifest could not be serialized",
        )
    })?;
    let manifest_hash = ContentHash::from_bytes(&manifest_bytes);
    std::fs::write(output_dir.join("manifest.json"), &manifest_bytes)?;
    files.push(BundleFile {
        name: "manifest.json".to_owned(),
        byte_len: u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX),
        content_hash: manifest_hash.clone(),
    });

    redacted_fields.sort();
    redacted_fields.dedup();
    Ok(SupportBundle {
        output_dir: output_dir.to_string_lossy().into_owned(),
        manifest_hash,
        files,
        redacted_fields,
    })
}

/// The commands that reproduce what the bundle reports.
///
/// A bundle without these is a screenshot: a reader can see the numbers and
/// cannot check them. The paths are the caller's own, quoted for a shell that
/// takes them literally.
#[must_use]
pub fn reproducible_commands(data_dir: &Path) -> String {
    let dir = data_dir.to_string_lossy().into_owned();
    format!(
        "# Reproducing this bundle\n\
         \n\
         Every number in `host.json` came from one of these commands, run against\n\
         the same data directory. The bundle carries no transcript and no\n\
         credential: to inspect those, run the commands yourself on the host.\n\
         \n\
         ```\n\
         ha maintenance doctor --data-dir \"{dir}\" --json\n\
         ha maintenance release-matrix --json\n\
         ha maintenance tombstones --data-dir \"{dir}\" --json\n\
         ha maintenance verify-backup --backup <backup-dir> --json\n\
         ```\n\
         \n\
         `host.json` fields map to them as follows:\n\
         \n\
         | field | command |\n\
         |---|---|\n\
         | `sqlite`, `schema_revisions`, `counts`, `retention` | `maintenance doctor` |\n\
         | `correlation_refs` | `ha status --session <id> --json` for the named session |\n\
         | `config` | the config file the host was started with, redacted |\n"
    )
}

fn write_bundle_file(
    output_dir: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<BundleFile, MaintenanceError> {
    let path: PathBuf = output_dir.join(name);
    std::fs::write(&path, bytes)?;
    Ok(BundleFile {
        name: name.to_owned(),
        byte_len: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        content_hash: ContentHash::from_bytes(bytes),
    })
}
