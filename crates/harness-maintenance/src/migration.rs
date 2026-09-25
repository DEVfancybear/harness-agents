//! Upgrade and downgrade safeguards.
//!
//! Migration runs on a **copy**, never in place, so an interrupted migration
//! cannot damage the source. A store whose recorded schema revision is newer
//! than this binary supports is refused for writes with a typed error, while
//! read-only inspection keeps working so an operator can still diagnose it.

use std::path::{Path, PathBuf};

use harness_store_sqlite::{
    DELEGATION_SCHEMA_VERSION, MAINTENANCE_SCHEMA_VERSION, RUNTIME_SCHEMA_VERSION,
    STORE_SCHEMA_VERSION, SqliteStore, StorePaths, TOOLS_SCHEMA_VERSION, WriterOpenOptions,
};
use harness_types::{ErrorCode, HostId};

use crate::contracts::MaintenanceError;

/// Whether this binary can write a given store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreCompatibility {
    /// No store exists at the data directory yet, so this binary may create one.
    Uninitialized,
    /// Every recorded revision is supported; writes are safe.
    Writable,
    /// The store is older and will be migrated on open, but only in a copy.
    NeedsMigration { recorded: i64, supported: i64 },
    /// The store is newer than this binary; writes must be refused.
    TooNew {
        recorded: i64,
        supported: i64,
        surface: String,
    },
}

impl StoreCompatibility {
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        matches!(self, Self::Uninitialized | Self::Writable)
    }

    /// Read-only diagnosis stays available in every case, including `TooNew`.
    #[must_use]
    pub const fn inspection_allowed(&self) -> bool {
        true
    }

    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Uninitialized => {
                "no store exists at this data directory yet; this binary may create one".to_owned()
            }
            Self::Writable => "store schema revisions are supported".to_owned(),
            Self::NeedsMigration {
                recorded,
                supported,
            } => format!(
                "store revision {recorded} is older than the supported {supported}; migrate a copy first"
            ),
            Self::TooNew {
                recorded,
                supported,
                surface,
            } => format!(
                "store {surface} revision {recorded} is newer than the supported {supported}; writes are refused"
            ),
        }
    }
}

/// What a migration produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationOutcome {
    pub source: String,
    pub destination: String,
    pub migrated: bool,
    pub revisions: std::collections::BTreeMap<String, i64>,
}

/// Whether a data directory already holds a store.
///
/// This is the distinction `doctor` reports and `backup` enforces: a directory
/// with no database is uninitialized, not broken, and there is nothing to
/// snapshot in it yet.
#[must_use]
pub fn store_is_initialized(data_dir: impl AsRef<Path>) -> bool {
    StorePaths::new(data_dir.as_ref()).database_path.is_file()
}

/// Inspect a data directory and report whether this binary may write it.
pub async fn check_store_compatibility(
    data_dir: impl AsRef<Path>,
) -> Result<StoreCompatibility, MaintenanceError> {
    // A directory that holds no database is not a broken store: it is a store
    // that has not been created yet. Opening it read-only would fail with
    // `read_only_store` and hide that distinction from `doctor`, which is the
    // first command a new operator runs.
    if !store_is_initialized(data_dir.as_ref()) {
        return Ok(StoreCompatibility::Uninitialized);
    }
    let store = SqliteStore::open_read_only(data_dir.as_ref()).await?;
    let revisions = store.all_schema_revisions().await?;
    store.close().await?;
    for (surface, recorded, supported) in [
        (
            "store",
            revisions.get("store").copied().unwrap_or(0),
            STORE_SCHEMA_VERSION,
        ),
        (
            "runtime",
            revisions.get("runtime").copied().unwrap_or(0),
            RUNTIME_SCHEMA_VERSION,
        ),
        (
            "tools",
            revisions.get("tools").copied().unwrap_or(0),
            TOOLS_SCHEMA_VERSION,
        ),
        (
            "delegation",
            revisions.get("delegation").copied().unwrap_or(0),
            DELEGATION_SCHEMA_VERSION,
        ),
        (
            "maintenance",
            revisions.get("maintenance").copied().unwrap_or(0),
            MAINTENANCE_SCHEMA_VERSION,
        ),
    ] {
        if recorded > supported {
            return Ok(StoreCompatibility::TooNew {
                recorded,
                supported,
                surface: surface.to_owned(),
            });
        }
    }
    let recorded = revisions.get("store").copied().unwrap_or(0);
    if recorded > 0 && recorded < STORE_SCHEMA_VERSION {
        return Ok(StoreCompatibility::NeedsMigration {
            recorded,
            supported: STORE_SCHEMA_VERSION,
        });
    }
    Ok(StoreCompatibility::Writable)
}

/// Migrate a store **into a new directory**, leaving the source untouched.
///
/// Opening the copy writable runs the ordinary migrations, so the migrated
/// result is produced by the same code path a normal open uses. An interrupted
/// run leaves the source readable and unchanged, and the incomplete copy can be
/// discarded.
pub async fn migrate_copy(
    data_dir: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<MigrationOutcome, MaintenanceError> {
    let source = data_dir.as_ref().to_path_buf();
    let destination = destination.as_ref().to_path_buf();
    if !source.join("harness.sqlite3").is_file() {
        return Err(MaintenanceError::new(
            ErrorCode::MigrationFailed,
            format!("no store at {}", source.display()),
        ));
    }
    if destination.join("harness.sqlite3").exists() {
        return Err(MaintenanceError::new(
            ErrorCode::RestoreTargetConflict,
            format!(
                "{} already holds a store; migrate into a fresh directory",
                destination.display()
            ),
        ));
    }
    std::fs::create_dir_all(&destination)?;
    copy_directory(&source, &destination)?;

    // Opening writable runs the migrations against the copy only.
    let store =
        SqliteStore::open_writer(WriterOpenOptions::new(&destination, HostId::generate())).await?;
    let revisions = store.all_schema_revisions().await?;
    store.close().await?;

    Ok(MigrationOutcome {
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        migrated: true,
        revisions,
    })
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), MaintenanceError> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        // The writer lock is process state, not data, and must not be copied.
        if name == "writer.lock" {
            continue;
        }
        let target: PathBuf = destination.join(&name);
        if path.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_directory(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}
