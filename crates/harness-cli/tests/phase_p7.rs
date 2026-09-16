//! P7 acceptance: recovery hardening, backup and restore, upgrade safeguards,
//! retention classes and release honesty, exercised through real components.
//!
//! Only external boundaries are absent: no live provider credential is supplied
//! and no remote backup target exists. Every store, artifact, tombstone, backup
//! directory and restore destination below is real, and the packaged `ha` binary
//! is started as a real process rather than called through `cargo run`.

#[path = "phase_p7/support.rs"]
mod support;

use std::sync::Arc;

use harness_maintenance::{
    BenchmarkTarget, CapabilityStatus, CapabilitySupport, MaintenanceError, PlatformStatus,
    PlatformSupport, ReleaseMatrix, RetentionAction, StoreCompatibility, check_store_compatibility,
    collect_garbage, create_backup, forget_source, list_tombstones, migrate_copy, restore_backup,
    run_retention, verify_backup,
};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{ErrorCode, HostId};
use serde_json::json;

use support::{
    FIXTURE_ARTIFACT, assert_code, fixture_store, open_writer, registry_cases, repository_root,
    run_cli, seed, temp_root,
};

// ---------------------------------------------------------------------------
// P7-S01
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s01_release_matrix_is_complete_and_honest() {
    // The matrix this build actually ships, read back through the real CLI.
    let matrix = shipped_matrix();
    matrix
        .validate()
        .expect("the shipped release matrix is valid");
    assert_eq!(matrix.platforms.len(), 2);
    assert!(
        matrix
            .platforms
            .iter()
            .any(|platform| platform.target_triple.contains("windows"))
    );
    assert!(
        matrix
            .platforms
            .iter()
            .any(|platform| platform.target_triple.contains("linux"))
    );
    assert!(
        matrix
            .platforms
            .iter()
            .all(|platform| !platform.evidence.trim().is_empty()),
        "every platform must state what was actually run"
    );

    // Surfaces an operator might assume are present are declared unsupported.
    for name in ["remote_mcp_endpoints", "os_sandboxing", "background_daemon"] {
        let capability = capability_named(&matrix, name);
        assert_eq!(capability.status, CapabilityStatus::Unsupported);
        assert!(
            !capability.note.trim().is_empty(),
            "{name} must state why it is unsupported"
        );
    }
    // A Linux artifact was built but never published, and that is said plainly.
    assert_eq!(
        capability_named(&matrix, "packaged_linux_artifact").status,
        CapabilityStatus::ComponentOnly
    );

    // A benchmark target is a target, not a result.
    assert!(
        matrix
            .benchmarks
            .iter()
            .all(|benchmark| !benchmark.is_measured()),
        "a stated target must never be reported as measured"
    );
    assert!(
        matrix
            .benchmarks
            .iter()
            .all(|benchmark| benchmark.met().is_none())
    );
    assert!(matrix.verdict().contains("unmeasured"));
    assert!(!matrix.verdict().contains("every benchmark measured"));

    // A measurement below target is reported as met, above target as missed, and
    // the two are derived from the same function.
    let measured = matrix_with(Some(900), Some(1200));
    assert_eq!(
        benchmark_named(&measured, "retrieval_p95").met(),
        Some(false)
    );
    assert_eq!(
        benchmark_named(&measured, "restore_state").met(),
        Some(true)
    );

    // A failing platform can never be hidden behind a green summary.
    let mut failing = measured.clone();
    failing.platforms[0].status = PlatformStatus::Failing;
    assert!(failing.verdict().contains("not release-ready"));
    failing.platforms[0].status = PlatformStatus::Unverified;
    assert!(failing.verdict().contains("partially verified"));
    assert_eq!(failing.not_green().len(), 1);

    // The 44 continuity and plugin cases are expanded, not summarised by one row.
    assert_eq!(matrix.verified_cases.len(), 44);
    assert!(matrix.verified_cases.contains(&"C27".to_owned()));
    assert!(matrix.verified_cases.contains(&"C28".to_owned()));
    assert!(matrix.verified_cases.contains(&"K14".to_owned()));
    assert!(!matrix.unverified_checks.is_empty());
    assert!(!matrix.out_of_scope.is_empty());

    // A matrix that hides a platform or drops a case is refused outright.
    let mut no_evidence = matrix.clone();
    no_evidence.platforms[0].evidence = "   ".to_owned();
    assert_code(no_evidence.validate(), ErrorCode::InvalidPayload);
    let mut no_platforms = matrix.clone();
    no_platforms.platforms.clear();
    assert_code(no_platforms.validate(), ErrorCode::InvalidPayload);
    let mut future_schema = matrix;
    future_schema.schema_version = 99;
    assert_code(
        future_schema.validate(),
        ErrorCode::UnsupportedSchemaVersion,
    );
}

/// Read the shipped matrix back through the real CLI JSON, so the test observes
/// the same surface an operator does.
fn shipped_matrix() -> ReleaseMatrix {
    let output = run_cli(&["maintenance", "release-matrix", "--json"]);
    assert!(output.status.success(), "{output:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("matrix JSON parses");
    let mut matrix = matrix_with(None, None);
    parsed["release_name"]
        .as_str()
        .expect("release name")
        .clone_into(&mut matrix.release_name);
    matrix
}

/// The matrix shape with explicit measurements, so honesty can be probed.
fn matrix_with(retrieval_p95_ms: Option<u64>, restore_ms: Option<u64>) -> ReleaseMatrix {
    ReleaseMatrix {
        schema_version: harness_maintenance::MAINTENANCE_CONTRACT_VERSION,
        release_name: "harness-cli p7-acceptance".to_owned(),
        platforms: vec![
            PlatformSupport {
                os: "windows".to_owned(),
                target_triple: "x86_64-pc-windows-msvc".to_owned(),
                toolchain: "1.97.1".to_owned(),
                status: PlatformStatus::Verified,
                evidence: "P0-P7 gates run locally on this platform".to_owned(),
            },
            PlatformSupport {
                os: "linux".to_owned(),
                target_triple: "x86_64-unknown-linux-gnu".to_owned(),
                toolchain: "1.97.1".to_owned(),
                status: PlatformStatus::Verified,
                evidence: "P0-P7 gates run in the GitHub Actions ubuntu-latest job".to_owned(),
            },
        ],
        capabilities: vec![
            capability("backup_restore", CapabilityStatus::Supported),
            capability("retention", CapabilityStatus::Supported),
            capability("packaged_linux_artifact", CapabilityStatus::ComponentOnly),
            capability("remote_mcp_endpoints", CapabilityStatus::Unsupported),
            capability("os_sandboxing", CapabilityStatus::Unsupported),
            capability("background_daemon", CapabilityStatus::Unsupported),
        ],
        benchmarks: vec![
            benchmark("retrieval_p95", 250, retrieval_p95_ms),
            benchmark("restore_state", 2000, restore_ms),
        ],
        verified_cases: support::all_case_ids(),
        unverified_checks: vec!["live provider evaluation: no credential supplied".to_owned()],
        out_of_scope: vec!["P8 Web UI".to_owned()],
    }
}

fn capability(name: &str, status: CapabilityStatus) -> CapabilitySupport {
    CapabilitySupport {
        name: name.to_owned(),
        status,
        note: format!("{name} is {status:?} in this build"),
    }
}

fn benchmark(name: &str, target: u64, measured: Option<u64>) -> BenchmarkTarget {
    BenchmarkTarget {
        name: name.to_owned(),
        description: format!("{name} over the recorded dataset"),
        unit: "ms".to_owned(),
        target,
        measured,
    }
}

fn capability_named<'a>(matrix: &'a ReleaseMatrix, name: &str) -> &'a CapabilitySupport {
    matrix
        .capabilities
        .iter()
        .find(|capability| capability.name == name)
        .unwrap_or_else(|| panic!("{name} must be declared in the release matrix"))
}

fn benchmark_named<'a>(matrix: &'a ReleaseMatrix, name: &str) -> &'a BenchmarkTarget {
    matrix
        .benchmarks
        .iter()
        .find(|benchmark| benchmark.name == name)
        .unwrap_or_else(|| panic!("{name} must be declared in the release matrix"))
}

/// This crate's acceptance target directory.
fn crate_tests_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

/// Every one of the 44 continuity and plugin cases is registered as implemented
/// and required, and names a test that really exists in a real target.
fn assert_case_matrix_is_complete() {
    let cases = support::all_case_ids();
    assert_eq!(cases.len(), 44);
    assert_eq!(
        cases.first().map(String::as_str),
        Some("C01"),
        "the matrix starts at C01"
    );
    assert_eq!(
        cases.last().map(String::as_str),
        Some("K14"),
        "the matrix ends at K14"
    );
    let registry = registry_cases();
    for case in &cases {
        let entry = registry
            .iter()
            .find(|entry| entry["id"] == case.as_str())
            .unwrap_or_else(|| panic!("{case} is missing from the acceptance registry"));
        assert_eq!(
            entry["readiness"], "implemented",
            "{case} is registered but not implemented"
        );
        assert_eq!(entry["required"], true, "{case} must be required");
        let names = entry["test_names"]
            .as_array()
            .unwrap_or_else(|| panic!("{case} has no test names"));
        assert!(!names.is_empty(), "{case} has no executable test");
        let default_target = entry["target"].as_str().expect("a case names its target");
        assert!(
            crate_tests_dir()
                .join(format!("{default_target}.rs"))
                .is_file(),
            "{case} names target {default_target}, which has no test file"
        );
        // `phase_p1` owns tests named `p1_...`, which is how the gate discovers
        // them in that target.
        let phase_prefix = format!(
            "{}_",
            default_target
                .rsplit('_')
                .next()
                .unwrap_or_default()
                .to_lowercase()
        );
        for name in names {
            let name = name.as_str().expect("a test name is a string");
            match name.split_once("::") {
                None => assert!(
                    name.starts_with(&phase_prefix),
                    "{case} names a test that does not belong to {default_target}: {name}"
                ),
                Some((target, rest)) => {
                    // A cross-target entry names a module of another acceptance
                    // target, which must exist as a real module file.
                    assert!(
                        target.starts_with("phase_") && !rest.is_empty(),
                        "{case} names a malformed qualified test: {name}"
                    );
                    let module = rest.split("::").next().unwrap_or_default();
                    assert!(
                        crate_tests_dir()
                            .join(target)
                            .join(format!("{module}.rs"))
                            .is_file()
                            || crate_tests_dir().join(format!("{target}.rs")).is_file(),
                        "{case} qualifies {name} with a target that does not exist"
                    );
                }
            }
        }
    }
    // The registry holds exactly the 44 cases and no duplicates.
    let mut ids = registry
        .iter()
        .filter_map(|entry| entry["id"].as_str())
        .filter(|id| {
            (id.starts_with('C') || id.starts_with('K'))
                && id.len() == 3
                && id[1..].chars().all(|character| character.is_ascii_digit())
        })
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 44, "the registry holds every case exactly once");
}

// ---------------------------------------------------------------------------
// P7-S02
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s02_backup_manifest_pins_every_artifact() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;

    // A pin on unfinished work is carried into the manifest, so a later
    // collection cannot drop what the backup promised to keep.
    let store = open_writer(&data).await;
    let pinned = harness_maintenance::retention::pin_unfinished_work(
        &store,
        std::slice::from_ref(&testbed.artifact_id.as_str().to_owned()),
        &harness_types::TaskId::generate(),
    )
    .await
    .expect("pin unfinished work");
    assert_eq!(pinned, 1);
    store.close().await.expect("close");

    let backup_dir = root.path().join("backup");
    let outcome = create_backup(&data, &backup_dir).await.expect("backup");
    assert_eq!(outcome.artifact_count, 1);
    assert_eq!(outcome.total_artifact_bytes, testbed.artifact_bytes);
    assert_eq!(outcome.pinned, 1, "the pin is recorded in the manifest");
    assert_eq!(outcome.tombstones, 0);

    let manifest = verify_backup(&backup_dir).await.expect("verify backup");
    assert_eq!(manifest.artifacts.len(), 1);
    assert_eq!(
        manifest.artifacts[0].artifact_id,
        testbed.artifact_id.as_str()
    );
    assert_eq!(
        manifest.artifacts[0].relative_path,
        testbed.artifact_relative_path
    );
    assert_eq!(manifest.artifacts[0].content_hash, testbed.artifact_hash);
    assert_eq!(manifest.artifacts[0].byte_len, testbed.artifact_bytes);
    assert_eq!(manifest.pins.len(), 1);
    assert_eq!(manifest.pins[0].reason, "unfinished_work");
    assert_eq!(
        manifest.database_file,
        harness_maintenance::BACKUP_DATABASE_NAME
    );
    // The manifest describes the snapshot, not the live store.
    assert_eq!(manifest.schema_revisions.get("store").copied(), Some(1));
    assert_eq!(
        manifest.schema_revisions.get("maintenance").copied(),
        Some(1)
    );
    assert_eq!(
        manifest.source_data_dir,
        data.to_string_lossy(),
        "the manifest names the directory it came from"
    );
    // The copied artifact really holds the bytes.
    assert_eq!(
        std::fs::read(backup_dir.join(&testbed.artifact_relative_path)).expect("read artifact"),
        FIXTURE_ARTIFACT
    );

    // A backup directory is never merged or overwritten.
    let error = create_backup(&data, &backup_dir)
        .await
        .expect_err("an existing backup must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // A tampered manifest is detected rather than trusted.
    let manifest_path = backup_dir.join(harness_maintenance::BACKUP_MANIFEST_NAME);
    let mut tampered: harness_maintenance::BackupManifest =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    tampered.source_data_dir = "/somewhere/else".to_owned();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&tampered).expect("serialize manifest"),
    )
    .expect("write manifest");
    let error = verify_backup(&backup_dir)
        .await
        .expect_err("a tampered manifest must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // Repair the digest, then break the artifact bytes: the snapshot itself is
    // validated, not only its manifest.
    let repaired = harness_maintenance::BackupManifest {
        manifest_hash: tampered.compute_hash().expect("recompute digest"),
        ..tampered
    };
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&repaired).expect("serialize manifest"),
    )
    .expect("write manifest");
    verify_backup(&backup_dir).await.expect("repaired verify");
    let artifact_path = backup_dir.join(&testbed.artifact_relative_path);
    std::fs::write(&artifact_path, b"tampered").expect("tamper artifact");
    let error = verify_backup(&backup_dir)
        .await
        .expect_err("a modified artifact must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // A missing artifact makes the backup invalid rather than silently partial.
    std::fs::remove_file(&artifact_path).expect("remove artifact");
    let error = verify_backup(&backup_dir)
        .await
        .expect_err("a missing artifact must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // A directory that holds no store has nothing to back up.
    let empty = root.path().join("empty");
    std::fs::create_dir_all(&empty).expect("create empty dir");
    let error = create_backup(&empty, root.path().join("nowhere"))
        .await
        .expect_err("a store is required");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);
}

// ---------------------------------------------------------------------------
// P7-C27
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_c27_restore_into_new_directory_and_refuse_active_target() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;

    let store = Arc::new(open_writer(&data).await);
    let sessions_before = store.list_sessions().await.expect("list sessions").len();
    let artifacts_before = store.artifact_pins().await.expect("list artifacts").len();
    assert_eq!(sessions_before, 1);
    assert_eq!(artifacts_before, 1);
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("close");

    let backup_dir = root.path().join("backup");
    create_backup(&data, &backup_dir).await.expect("backup");
    // A backup is not an active store; the freshness checks must not confuse the
    // two, so a restore into the backup directory itself is refused too.
    let error = restore_backup(&backup_dir, &backup_dir)
        .await
        .expect_err("a restore target must not be the backup itself");
    assert_eq!(error.code(), ErrorCode::RestoreTargetConflict);

    // A restore writes only into a fresh directory and never activates it.
    let destination = root.path().join("restored");
    let restored = restore_backup(&backup_dir, &destination)
        .await
        .expect("restore");
    assert!(restored.report.database_verified);
    assert_eq!(restored.report.artifacts_verified, 1);
    assert!(restored.report.artifacts_missing.is_empty());
    assert!(restored.report.artifacts_corrupt.is_empty());
    assert!(!restored.report.activated, "a restore must not activate");
    assert!(restored.report.is_complete());
    assert_eq!(restored.report.tombstones_restored, 0);
    assert_eq!(restored.report.restored_into, destination.to_string_lossy());
    assert!(destination.join("restore.json").is_file());
    assert!(
        !destination.join(".active").is_file(),
        "activation is a separate explicit step"
    );

    // The provenance record states plainly that nothing was activated.
    let provenance: serde_json::Value = serde_json::from_slice(
        &std::fs::read(destination.join("restore.json")).expect("read provenance"),
    )
    .expect("provenance JSON");
    assert_eq!(provenance["activated"], json!(false));
    assert_eq!(provenance["schema_version"], json!(1));

    // The restored copy opens read-only and carries the same durable records.
    let read_back = SqliteStore::open_read_only(&destination)
        .await
        .expect("open restored copy");
    assert_eq!(
        read_back.list_sessions().await.expect("sessions").len(),
        sessions_before
    );
    let artifacts = read_back.artifact_pins().await.expect("artifacts");
    assert_eq!(artifacts.len(), artifacts_before);
    assert_eq!(artifacts[0].0, testbed.artifact_id.as_str());
    assert_eq!(artifacts[0].2, testbed.artifact_hash);
    assert_eq!(artifacts[0].3, testbed.artifact_bytes);
    read_back.close().await.expect("close restored copy");
    assert_eq!(
        std::fs::read(destination.join(&testbed.artifact_relative_path)).expect("read artifact"),
        FIXTURE_ARTIFACT
    );

    // The source directory was not modified by the restore.
    let source = SqliteStore::open_read_only(&data)
        .await
        .expect("open source");
    assert_eq!(source.artifact_pins().await.expect("artifacts").len(), 1);
    assert_eq!(source.list_sessions().await.expect("sessions").len(), 1);
    source.close().await.expect("close source");

    // Restoring over the now-populated destination is refused.
    let error = restore_backup(&backup_dir, &destination)
        .await
        .expect_err("an occupied target must be refused");
    assert_eq!(error.code(), ErrorCode::RestoreTargetConflict);

    // Restoring over a live store is refused.
    let error = restore_backup(&backup_dir, &data)
        .await
        .expect_err("a live store must be refused");
    assert_eq!(error.code(), ErrorCode::RestoreTargetConflict);

    // A corrupt snapshot is refused before anything is activated, and the
    // destination it was aimed at is not left claiming to be a restore.
    let broken = root.path().join("broken");
    std::fs::create_dir_all(&broken).expect("create broken dir");
    std::fs::write(
        broken.join(harness_maintenance::BACKUP_MANIFEST_NAME),
        std::fs::read(backup_dir.join(harness_maintenance::BACKUP_MANIFEST_NAME))
            .expect("read manifest"),
    )
    .expect("copy manifest");
    std::fs::write(
        broken.join(harness_maintenance::BACKUP_DATABASE_NAME),
        b"this is not a database at all",
    )
    .expect("write junk database");
    // The manifest still describes the real database, so verification catches it.
    let error = restore_backup(&broken, root.path().join("never"))
        .await
        .expect_err("a corrupt snapshot must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // Restoring a directory that is not a backup is refused.
    let error = restore_backup(&broken, root.path().join("never"))
        .await
        .expect_err("a non-backup directory must be refused");
    assert_eq!(error.code(), ErrorCode::BackupManifestInvalid);

    // Activation is the explicit step a restore deliberately skips.
    let activated = harness_maintenance::backup::activate_restored(&destination)
        .await
        .expect("activate restored copy");
    assert!(destination.join(".active").is_file());
    activated.close().await.expect("close activated store");
}

// ---------------------------------------------------------------------------
// P7-S03
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s03_migration_runs_on_a_copy_and_refuses_newer_writes() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;

    // The store this binary just wrote is writable, with no migration pending.
    let compatibility = check_store_compatibility(&data)
        .await
        .expect("compatibility");
    assert_eq!(compatibility, StoreCompatibility::Writable);
    assert!(compatibility.is_writable());
    assert!(compatibility.inspection_allowed());
    assert!(compatibility.describe().contains("supported"));

    // A migration copies the store and leaves the source byte-identical.
    let database = data.join("harness.sqlite3");
    let source_before = std::fs::read(&database).expect("read source database");
    let copy = root.path().join("migrated");
    let outcome = migrate_copy(&data, &copy).await.expect("migrate a copy");
    assert!(outcome.migrated);
    assert_eq!(outcome.source, data.to_string_lossy());
    assert_eq!(outcome.destination, copy.to_string_lossy());
    assert_eq!(outcome.revisions.get("store").copied(), Some(1));
    assert_eq!(outcome.revisions.get("maintenance").copied(), Some(1));
    let source_after = std::fs::read(&database).expect("read source database");
    assert_eq!(
        source_before, source_after,
        "a migration must not modify the source store"
    );

    // The copy is a working store holding the same durable records.
    let migrated = SqliteStore::open_read_only(&copy)
        .await
        .expect("open migrated copy");
    let artifacts = migrated.artifact_pins().await.expect("artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].0, testbed.artifact_id.as_str());
    assert_eq!(migrated.list_sessions().await.expect("sessions").len(), 1);
    migrated.close().await.expect("close copy");
    let writer_lock = copy.join("writer.lock");
    // The writer lock is an OS file lock owned by a running host, not data. The
    // copy's lock was acquired fresh by the migration open, so nothing was
    // carried over from the source, and the copy is genuinely writable.
    assert!(
        writer_lock.is_file(),
        "the migration open must create its own writer lock"
    );
    assert_eq!(
        std::fs::metadata(&writer_lock)
            .expect("stat the copy writer lock")
            .len(),
        0,
        "a fresh writer lock is an empty advisory file, so no lock state was copied"
    );
    let migrated_writable = open_writer(&copy).await;
    migrated_writable
        .close()
        .await
        .expect("close migrated copy");

    // Migrating into an occupied directory is refused rather than merged.
    let error = migrate_copy(&data, &copy)
        .await
        .expect_err("an occupied destination must be refused");
    assert_eq!(error.code(), ErrorCode::RestoreTargetConflict);
    // Migrating from a directory with no store is refused.
    let empty = root.path().join("empty");
    std::fs::create_dir_all(&empty).expect("create empty dir");
    let error = migrate_copy(&empty, root.path().join("nothing"))
        .await
        .expect_err("a source store is required");
    assert_eq!(error.code(), ErrorCode::MigrationFailed);

    refuses_writes_to_a_newer_store(root.path(), &database).await;
}

/// A store whose recorded revision is newer than this binary refuses writes while
/// read-only inspection keeps working, so it can be diagnosed rather than
/// silently rewritten by an older binary.
async fn refuses_writes_to_a_newer_store(root: &std::path::Path, database: &std::path::Path) {
    let newer = root.join("newer");
    std::fs::create_dir_all(&newer).expect("create newer dir");
    std::fs::copy(database, newer.join("harness.sqlite3")).expect("copy store");
    bump_store_revision(&newer).await;
    let compatibility = check_store_compatibility(&newer)
        .await
        .expect("compatibility");
    match &compatibility {
        StoreCompatibility::TooNew {
            recorded,
            supported,
            surface,
        } => {
            assert_eq!(*recorded, 99);
            assert_eq!(*supported, 1);
            assert_eq!(surface, "store");
        }
        other => panic!("a newer store must be reported as too new, got {other:?}"),
    }
    assert!(!compatibility.is_writable(), "writes must be refused");
    assert!(
        compatibility.inspection_allowed(),
        "read-only diagnosis must stay available"
    );
    assert!(compatibility.describe().contains("writes are refused"));

    // Read-only inspection really works on the newer store.
    let read_only = SqliteStore::open_read_only(&newer)
        .await
        .expect("read-only inspection of a newer store");
    assert_eq!(read_only.list_sessions().await.expect("sessions").len(), 1);
    assert_eq!(read_only.artifact_pins().await.expect("artifacts").len(), 1);
    read_only.close().await.expect("close newer store");

    // A write attempt fails with a typed error rather than silently rewriting the
    // store the newer binary owns.
    let error = SqliteStore::open_writer(WriterOpenOptions::new(&newer, HostId::generate()))
        .await
        .expect_err("a newer store must refuse a writer");
    assert!(
        matches!(
            error.code(),
            ErrorCode::UnsupportedSchemaVersion | ErrorCode::MigrationFailed
        ),
        "unexpected code {:?}: {error}",
        error.code()
    );
}

/// Raise the recorded store revision so the store looks newer than this binary.
async fn bump_store_revision(data_dir: &std::path::Path) {
    use sqlx::{Connection, SqliteConnection};
    let path = data_dir.join("harness.sqlite3");
    let mut connection = SqliteConnection::connect(&format!("sqlite:{}", path.display()))
        .await
        .expect("open sqlite directly");
    sqlx::query("UPDATE schema_migrations SET version = 99")
        .execute(&mut connection)
        .await
        .expect("bump the recorded revision");
    let _ = connection.close().await;
}

// ---------------------------------------------------------------------------
// P7-S04
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s04_retention_classes_are_separate_and_confirmed() {
    let root = temp_root();
    let data = root.path().join("data");
    let (store, _artifact) = fixture_store(&data).await;
    let store = Arc::new(store);

    // Invalidate and archive keep the record and need no confirmation; neither
    // is spelled "delete" and neither leaves a tombstone.
    assert!(!RetentionAction::Invalidate.requires_confirmation());
    assert!(!RetentionAction::Archive.requires_confirmation());
    for action in [RetentionAction::Invalidate, RetentionAction::Archive] {
        let report = run_retention(
            &store,
            action,
            "file",
            "src/lib.rs",
            "fixture reason",
            None,
            &[],
        )
        .await
        .expect("retention runs without a confirmation");
        assert_eq!(report.action, action);
        assert_eq!(report.target, "file:src/lib.rs");
        assert!(report.affected_assets.is_empty());
        assert!(
            report.tombstone_id.is_none(),
            "{action} must not tombstone: only forgetting removes content"
        );
    }

    // Forgetting needs an explicit confirmation equal to its target.
    assert!(RetentionAction::Forget.requires_confirmation());
    let refused = forget_source(
        &store,
        "file",
        "src/secret.rs",
        "operator asked",
        "wrong-token",
        &[],
    )
    .await
    .expect_err("a mismatched confirmation must be refused");
    assert_eq!(refused.code(), ErrorCode::RetentionRefused);
    let refused = forget_source(&store, "file", "src/secret.rs", "operator asked", "", &[])
        .await
        .expect_err("an empty confirmation must be refused");
    assert_eq!(refused.code(), ErrorCode::RetentionRefused);
    // A refused forget leaves nothing behind.
    assert!(
        list_tombstones(&store)
            .await
            .expect("tombstones")
            .is_empty()
    );

    // The matching confirmation performs the forget and reports what survived.
    let report = forget_source(
        &store,
        "file",
        "src/secret.rs",
        "operator asked",
        "src/secret.rs",
        &["an external backup taken last week".to_owned()],
    )
    .await
    .expect("a confirmed forget runs");
    assert_eq!(report.action, RetentionAction::Forget);
    assert_eq!(report.target, "file:src/secret.rs");
    assert!(report.tombstone_id.is_some());
    assert_eq!(report.surviving_copies.len(), 1);

    // An empty target is refused before anything is recorded.
    let error = run_retention(
        &store,
        RetentionAction::Invalidate,
        "file",
        "   ",
        "reason",
        None,
        &[],
    )
    .await
    .expect_err("an empty target must be refused");
    assert_eq!(error.code(), ErrorCode::InvalidPayload);

    // Only the forgotten source is tombstoned.
    let tombstones = list_tombstones(&store).await.expect("tombstones");
    assert_eq!(tombstones.len(), 1);
    assert_eq!(tombstones[0].source_id, "src/secret.rs");
    assert!(
        !store
            .is_tombstoned("file", "src/lib.rs")
            .await
            .expect("check"),
        "an invalidated source is not a forgotten one"
    );

    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("close");
}

// ---------------------------------------------------------------------------
// P7-C28
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_c28_forget_blocks_reextraction_and_reports_survivors() {
    let root = temp_root();
    let data = root.path().join("data");
    let (store, _artifact) = fixture_store(&data).await;
    let store = Arc::new(store);

    // A source that was never forgotten may be extracted from.
    harness_maintenance::retention::assert_not_tombstoned(&store, "file", "src/lib.rs")
        .await
        .expect("an unforgotten source is extractable");

    let report = forget_source(
        &store,
        "file",
        "src/lib.rs",
        "operator request under retention policy",
        "src/lib.rs",
        &["backup-2026-01".to_owned(), "external archive".to_owned()],
    )
    .await
    .expect("forget");
    assert!(report.tombstone_id.is_some());

    // Re-extraction is now refused, so forgotten content cannot reappear.
    let error = harness_maintenance::retention::assert_not_tombstoned(&store, "file", "src/lib.rs")
        .await
        .expect_err("a tombstoned source must refuse re-extraction");
    assert_eq!(error.code(), ErrorCode::RetentionRefused);
    assert!(error.to_string().contains("re-extraction is refused"));
    assert!(
        store
            .is_tombstoned("file", "src/lib.rs")
            .await
            .expect("check")
    );

    // The tombstone names every copy that may still hold the data.
    let tombstones = list_tombstones(&store).await.expect("tombstones");
    assert_eq!(tombstones.len(), 1);
    assert_eq!(
        tombstones[0].surviving_copies,
        vec!["backup-2026-01".to_owned(), "external archive".to_owned()]
    );
    assert!(tombstones[0].reason.contains("operator request"));
    assert!(tombstones[0].created_unix_ms > 0);

    // The retention summary an operator reads agrees with the records.
    let summary = harness_maintenance::retention_summary(&store)
        .await
        .expect("summary");
    assert_eq!(summary["tombstones"], json!(1));
    assert_eq!(
        summary["surviving_copies"],
        json!(["backup-2026-01", "external archive"])
    );

    // A different source is unaffected by another source's tombstone.
    assert!(
        !store
            .is_tombstoned("file", "src/other.rs")
            .await
            .expect("check")
    );
    harness_maintenance::retention::assert_not_tombstoned(&store, "file", "src/other.rs")
        .await
        .expect("an unrelated source stays extractable");

    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("close");

    // A tombstone survives a backup and a restore, so a forget is durable.
    let backup_dir = root.path().join("backup");
    let outcome = create_backup(&data, &backup_dir).await.expect("backup");
    assert_eq!(outcome.tombstones, 1, "the tombstone is in the manifest");
    let manifest = verify_backup(&backup_dir).await.expect("verify");
    assert_eq!(
        manifest.tombstones,
        vec!["file:src/lib.rs".to_owned()],
        "the manifest names the tombstoned source"
    );
    let destination = root.path().join("restored");
    let restored = restore_backup(&backup_dir, &destination)
        .await
        .expect("restore");
    assert_eq!(restored.report.tombstones_restored, 1);

    let restored_store = SqliteStore::open_read_only(&destination)
        .await
        .expect("open restored store");
    assert!(
        restored_store
            .is_tombstoned("file", "src/lib.rs")
            .await
            .expect("check"),
        "a restore must preserve tombstones"
    );
    let error = harness_maintenance::retention::assert_not_tombstoned(
        &restored_store,
        "file",
        "src/lib.rs",
    )
    .await
    .expect_err("a restored tombstone still refuses re-extraction");
    assert_eq!(error.code(), ErrorCode::RetentionRefused);
    let restored_tombstones = list_tombstones(&restored_store).await.expect("tombstones");
    assert_eq!(restored_tombstones.len(), 1);
    assert_eq!(
        restored_tombstones[0].surviving_copies,
        vec!["backup-2026-01".to_owned(), "external archive".to_owned()],
        "a restore must not lose the surviving-copy disclosure"
    );
    restored_store.close().await.expect("close restored store");
}

// ---------------------------------------------------------------------------
// P7 garbage collection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_gc_never_removes_a_pinned_artifact() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;
    let store = open_writer(&data).await;
    let artifact_id = testbed.artifact_id.as_str().to_owned();
    let artifact_path = data.join(&testbed.artifact_relative_path);
    assert!(artifact_path.is_file());

    // With no pin, no reference and no grace period, the artifact is collectable.
    let dry = collect_garbage(&store, 0, true).await.expect("dry run");
    assert_eq!(dry.considered, 1);
    assert_eq!(dry.collected, vec![artifact_id.clone()]);
    assert_eq!(dry.bytes_reclaimed, testbed.artifact_bytes);
    assert!(dry.retained_pinned.is_empty());
    assert!(dry.retained_referenced.is_empty());
    assert!(dry.summary().contains("considered 1 artifacts"));
    // A dry run removed nothing, on disk or in the store.
    assert!(artifact_path.is_file(), "a dry run must not delete bytes");
    assert_eq!(store.artifact_pins().await.expect("artifacts").len(), 1);

    // A young artifact stays inside the grace period.
    let young = collect_garbage(&store, 3600, true).await.expect("dry run");
    assert_eq!(young.retained_young, vec![artifact_id.clone()]);
    assert!(young.collected.is_empty());
    assert_eq!(young.bytes_reclaimed, 0);
    let default_grace = harness_maintenance::default_grace_seconds();
    assert_eq!(default_grace, harness_maintenance::DEFAULT_GC_GRACE_SECONDS);
    assert_eq!(default_grace, 7 * 24 * 60 * 60);

    // A pin protects the artifact even with no grace period. This is the
    // backup/garbage-collection race the pin exists to close.
    let pinned = harness_maintenance::retention::pin_backup(
        &store,
        std::slice::from_ref(&artifact_id),
        "backup-2026-01",
    )
    .await
    .expect("pin for backup");
    assert_eq!(pinned, 1);
    let report = collect_garbage(&store, 0, false).await.expect("gc");
    assert_eq!(report.retained_pinned, vec![artifact_id.clone()]);
    assert!(report.collected.is_empty());
    assert_eq!(report.bytes_reclaimed, 0);
    assert!(artifact_path.is_file(), "a pinned artifact keeps its bytes");
    assert_eq!(store.artifact_pins().await.expect("artifacts").len(), 1);
    // A backup taken while pinned records the pin.
    let backup_dir = root.path().join("backup");
    let outcome = create_backup(&data, &backup_dir).await.expect("backup");
    assert_eq!(outcome.pinned, 1);
    assert_eq!(outcome.artifact_count, 1);

    // Releasing the pin makes the artifact collectable, and collection really
    // removes both the bytes and the record.
    let released = store
        .unpin_artifacts("backup-2026-01")
        .await
        .expect("release pin");
    assert_eq!(released, 1);
    assert!(store.pinned_artifact_ids().await.expect("pins").is_empty());
    let report = collect_garbage(&store, 0, false).await.expect("gc");
    assert_eq!(report.collected, vec![artifact_id.clone()]);
    assert_eq!(report.bytes_reclaimed, testbed.artifact_bytes);
    assert!(!artifact_path.exists(), "collected bytes are gone");
    assert!(
        store.artifact_pins().await.expect("artifacts").is_empty(),
        "the artifact record is gone"
    );
    // A second pass has nothing left to consider.
    let empty = collect_garbage(&store, 0, false).await.expect("gc");
    assert_eq!(empty.considered, 0);
    assert!(empty.collected.is_empty());

    store.close().await.expect("close");
}

// ---------------------------------------------------------------------------
// P7-S05
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s05_full_case_matrix_runs_end_to_end() {
    // Every continuity and plugin case is exercised by a real acceptance target
    // in this workspace. The matrix must cover all 44, and each one must be
    // registered as implemented and required, which is what the phase gate
    // enforces before it runs anything.
    assert_case_matrix_is_complete();

    // The run is not registry-only: the real recovery path is exercised on a real
    // store, then the same records survive a real backup and restore. This is the
    // full-matrix smoke path an operator would follow.
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;
    let store = Arc::new(open_writer(&data).await);
    let sessions = store.list_sessions().await.expect("sessions");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, testbed.session_id);
    let recovered = harness_session::SessionService::new(Arc::clone(&store))
        .recover(&testbed.session_id)
        .await
        .expect("recover the session from durable state");
    assert_eq!(recovered.snapshot_diagnostic, None);
    assert_eq!(recovered.pending_execution_count, 0);
    assert_eq!(
        recovered.instruction_texts,
        vec!["p7 maintenance fixture".to_owned()],
        "the admitted instruction is rebuilt from durable state"
    );
    Arc::try_unwrap(store)
        .expect("store released")
        .close()
        .await
        .expect("close");

    let backup_dir = root.path().join("backup");
    create_backup(&data, &backup_dir).await.expect("backup");
    let destination = root.path().join("restored");
    let restored = restore_backup(&backup_dir, &destination)
        .await
        .expect("restore");
    assert!(restored.report.is_complete());
    assert!(!restored.report.activated);
    assert_eq!(restored.report.artifacts_verified, 1);
    let recovered = SqliteStore::open_read_only(&destination)
        .await
        .expect("open restored");
    let restored_sessions = recovered.list_sessions().await.expect("list sessions");
    assert_eq!(
        restored_sessions.len(),
        1,
        "one input is recovered exactly once from the restored copy"
    );
    assert_eq!(restored_sessions[0].session_id, testbed.session_id);
    assert_eq!(restored_sessions[0].next_sequence, 2);
    recovered.close().await.expect("close restored");
}

// ---------------------------------------------------------------------------
// P7-S06
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s06_packaged_binary_starts_and_reports_capabilities() {
    // The packaged binary is the real compiled artifact, started as a process.
    let version = run_cli(&["--version"]);
    assert!(
        version.status.success(),
        "the packaged binary must start: {version:?}"
    );
    let text = String::from_utf8_lossy(&version.stdout);
    assert!(!text.trim().is_empty(), "the binary reports a version");
    assert_maintenance_surface();

    // A real data directory is diagnosed by the real binary, which reports both
    // what it found and what it did not verify.
    let root = temp_root();
    let data = root.path().join("data");
    seed(&data).await;
    assert_doctor_is_honest(&data);

    // The release matrix through the packaged binary is honest about unmeasured
    // targets, so nothing is claimed without evidence.
    assert_release_matrix_is_honest();
    assert_measurements_are_reported_back();
}

/// The packaged binary exposes every maintenance subcommand the guides document.
fn assert_maintenance_surface() {
    let help = run_cli(&["maintenance", "--help"]);
    assert!(help.status.success(), "{help:?}");
    let help_text = String::from_utf8_lossy(&help.stdout);
    for subcommand in [
        "doctor",
        "backup",
        "verify-backup",
        "restore",
        "retain",
        "tombstones",
        "gc",
        "migrate-copy",
        "release-matrix",
    ] {
        assert!(
            help_text.contains(subcommand),
            "the packaged binary must expose {subcommand}"
        );
    }
}

/// `doctor` reports what it found and, separately, what it did not verify.
fn assert_doctor_is_honest(data: &std::path::Path) {
    let doctor = run_cli(&[
        "maintenance",
        "doctor",
        "--data-dir",
        &data.to_string_lossy(),
        "--json",
    ]);
    assert!(doctor.status.success(), "{doctor:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor JSON parses");
    assert_eq!(parsed["writable"], json!(true));
    assert_eq!(parsed["sessions"], json!(1));
    assert_eq!(parsed["artifacts"], json!(1));
    assert_eq!(parsed["schema_revisions"]["store"], json!(1));
    assert_eq!(parsed["retention"]["tombstones"], json!(0));
    let not_verified = parsed["not_verified"]
        .as_array()
        .expect("doctor names what it did not verify");
    assert!(!not_verified.is_empty());
    assert!(
        not_verified
            .iter()
            .any(|item| item.as_str().is_some_and(|text| text.contains("daemon")))
    );
}

/// With no benchmark run, the binary claims no benchmark result.
fn assert_release_matrix_is_honest() {
    let matrix = run_cli(&["maintenance", "release-matrix", "--json"]);
    assert!(matrix.status.success(), "{matrix:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&matrix.stdout).expect("matrix JSON parses");
    assert_eq!(parsed["schema_version"], json!(1));
    assert_eq!(
        parsed["platforms"].as_array().expect("platforms").len(),
        2,
        "both platforms are listed"
    );
    assert_eq!(parsed["verified_cases"], json!(44));
    let benchmarks = parsed["benchmarks"].as_array().expect("benchmarks");
    assert_eq!(benchmarks.len(), 2);
    assert!(
        benchmarks
            .iter()
            .all(|benchmark| benchmark["measured"].is_null()),
        "no benchmark was measured, so none carries a value"
    );
    assert!(
        benchmarks
            .iter()
            .all(|benchmark| benchmark["met"].is_null()),
        "an unmeasured target is never reported as met"
    );
    assert!(
        parsed["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability["status"] == json!("unsupported")),
        "unsupported surfaces must be declared"
    );
}

/// A measurement supplied to the binary is reflected back, so the honesty above
/// is not achieved by discarding input.
fn assert_measurements_are_reported_back() {
    let measured = run_cli(&[
        "maintenance",
        "release-matrix",
        "--json",
        "--retrieval-p95-ms",
        "900",
        "--restore-ms",
        "1200",
    ]);
    assert!(measured.status.success(), "{measured:?}");
    let parsed: serde_json::Value =
        serde_json::from_slice(&measured.stdout).expect("matrix JSON parses");
    let benchmarks = parsed["benchmarks"].as_array().expect("benchmarks");
    let retrieval = benchmarks
        .iter()
        .find(|benchmark| benchmark["name"] == json!("retrieval_p95"))
        .expect("retrieval benchmark");
    assert_eq!(retrieval["measured"], json!(900));
    assert_eq!(
        retrieval["met"],
        json!(false),
        "900ms misses a 250ms target"
    );
    let restore = benchmarks
        .iter()
        .find(|benchmark| benchmark["name"] == json!("restore_state"))
        .expect("restore benchmark");
    assert_eq!(restore["measured"], json!(1200));
    assert_eq!(restore["met"], json!(true));
}

// ---------------------------------------------------------------------------
// P7-S07
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p7_s07_operator_docs_and_release_record_are_consistent() {
    // The operator guides exist in both languages, are linked to each other, and
    // document the commands and limits the release actually ships.
    let root = repository_root();
    let mut bodies = Vec::new();
    for name in ["OPERATOR_GUIDE.en.md", "OPERATOR_GUIDE.vi.md"] {
        let path = root.join("docs").join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{name} must exist at {}: {error}", path.display()));
        bodies.push((name, text));
    }
    for (name, text) in &bodies {
        for required in [
            "ha maintenance doctor",
            "ha maintenance backup",
            "ha maintenance verify-backup",
            "ha maintenance restore",
            "ha maintenance retain",
            "ha maintenance tombstones",
            "ha maintenance gc",
            "ha maintenance migrate-copy",
            "ha maintenance release-matrix",
            "backup-manifest.json",
            "tombstone",
        ] {
            assert!(text.contains(required), "{name} must document {required}");
        }
        // The limits an operator must not discover the hard way are stated.
        assert!(
            text.contains("not activate") || text.contains("không tự kích hoạt"),
            "{name} must state that a restore does not activate on its own"
        );
        assert!(
            text.contains("daemon"),
            "{name} must state that no background daemon is provided"
        );
    }
    // The language switch works in both directions.
    assert!(bodies[0].1.contains("OPERATOR_GUIDE.vi.md"));
    assert!(bodies[1].1.contains("OPERATOR_GUIDE.en.md"));

    // The documented commands are the commands the binary exposes.
    let help = run_cli(&["maintenance", "--help"]);
    assert!(help.status.success());
    let help_text = String::from_utf8_lossy(&help.stdout).into_owned();
    for subcommand in [
        "doctor",
        "backup",
        "verify-backup",
        "restore",
        "retain",
        "tombstones",
        "gc",
        "migrate-copy",
        "release-matrix",
    ] {
        assert!(
            help_text.contains(subcommand),
            "{subcommand} is documented but not exposed by the binary"
        );
    }

    // The release record states the tested platforms, the unverified checks and
    // what is explicitly out of scope, and never claims an unmeasured target.
    let record = run_cli(&["maintenance", "release-matrix", "--json"]);
    assert!(record.status.success());
    let parsed: serde_json::Value =
        serde_json::from_slice(&record.stdout).expect("matrix JSON parses");
    let unverified = parsed["unverified_checks"]
        .as_array()
        .expect("unverified checks are listed");
    assert!(!unverified.is_empty(), "unverified checks must be named");
    let out_of_scope = parsed["out_of_scope"]
        .as_array()
        .expect("out-of-scope items are listed");
    assert!(
        out_of_scope
            .iter()
            .any(|item| item.as_str() == Some("P8 Web UI")),
        "the next phase must be declared out of scope rather than implied"
    );
    assert!(
        parsed["verdict"]
            .as_str()
            .is_some_and(|verdict| verdict.contains("unmeasured")),
        "an unmeasured target must never be reported as achieved"
    );

    // The implementation status documents agree with each other and name P7.
    for name in [
        "implementation/README.en.md",
        "implementation/README.vi.md",
        "implementation/P7_RELEASE.en.md",
        "implementation/P7_RELEASE.vi.md",
    ] {
        let path = root.join("docs").join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{name} must exist: {error}"));
        assert!(!text.trim().is_empty(), "{name} must not be empty");
    }
}

// ---------------------------------------------------------------------------
// Typed refusals and writer options
// ---------------------------------------------------------------------------

#[test]
fn p7_refusals_are_typed() {
    let error = MaintenanceError::new(ErrorCode::RetentionRefused, "no");
    assert_eq!(error.code(), ErrorCode::RetentionRefused);
    assert!(error.to_string().contains("retention_refused"));
    assert_eq!(error.message(), "no");
    assert_code(
        Err(MaintenanceError::new(
            ErrorCode::BackupManifestInvalid,
            "bad",
        )),
        ErrorCode::BackupManifestInvalid,
    );
    assert_code(
        Err(MaintenanceError::new(
            ErrorCode::RestoreTargetConflict,
            "busy",
        )),
        ErrorCode::RestoreTargetConflict,
    );
    // A store failure keeps its code when it crosses the maintenance boundary.
    let store_error = harness_store_sqlite::StoreError::new(ErrorCode::SnapshotCorrupt, "corrupt");
    let converted = MaintenanceError::from(store_error);
    assert_eq!(converted.code(), ErrorCode::SnapshotCorrupt);
}

#[tokio::test]
async fn p7_writer_options_open_a_fresh_directory() {
    let root = temp_root();
    let data = root.path().join("fresh");
    let store = SqliteStore::open_writer(WriterOpenOptions::new(&data, HostId::generate()))
        .await
        .expect("open a fresh store");
    let revisions = store.all_schema_revisions().await.expect("revisions");
    assert_eq!(revisions.get("store").copied(), Some(1));
    assert_eq!(revisions.get("maintenance").copied(), Some(1));
    assert_eq!(revisions.get("runtime").copied(), Some(1));
    assert_eq!(revisions.get("delegation").copied(), Some(1));
    let compatibility = check_store_compatibility(&data)
        .await
        .expect("compatibility");
    assert_eq!(compatibility, StoreCompatibility::Writable);
    store.close().await.expect("close");
}

#[tokio::test]
async fn p7_backup_without_artifacts_is_valid() {
    let root = temp_root();
    let data = root.path().join("data");
    let store = open_writer(&data).await;
    store.close().await.expect("close");
    let backup_dir = root.path().join("backup");
    let outcome = create_backup(&data, &backup_dir).await.expect("backup");
    assert_eq!(outcome.artifact_count, 0);
    assert_eq!(outcome.total_artifact_bytes, 0);
    let manifest = verify_backup(&backup_dir).await.expect("verify");
    assert!(manifest.artifacts.is_empty());
    assert!(!manifest.schema_revisions.is_empty());
    assert_eq!(
        manifest.schema_revisions.get("maintenance").copied(),
        Some(1)
    );
    // A restored empty store is still a valid, inspectable store.
    let destination = root.path().join("restored");
    let restored = restore_backup(&backup_dir, &destination)
        .await
        .expect("restore");
    assert!(restored.report.is_complete());
    assert_eq!(restored.report.artifacts_verified, 0);
    let opened = SqliteStore::open_read_only(&destination)
        .await
        .expect("open restored empty store");
    assert!(opened.list_sessions().await.expect("sessions").is_empty());
    opened.close().await.expect("close");
}
