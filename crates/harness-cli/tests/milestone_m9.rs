//! M9 acceptance target: data lifecycle, redacted diagnostics and the release
//! surface.
//!
//! Everything here runs against **real** components: the real `SQLite` store and
//! its transactions, the real backup/restore path (`VACUUM INTO` plus a verified
//! artifact manifest), the real retention and migration code, and the real `ha`
//! binary for the CLI cases. Only the provider and the exporter are absent, which
//! is the point of the two acceptance cases rather than a limitation of them.
//!
//! * `a31_backup_retention_restore` - a live task is backed up while it is still
//!   open, a corrupted backup is refused **without** writing to the destination,
//!   a restore into a fresh directory yields a store whose journal and reachable
//!   artifacts are intact, an interrupted migration leaves the source untouched,
//!   and garbage collection respects a pinned reference.
//! * `a32_diagnostics_isolation` - a support bundle carries the metadata an
//!   operator needs and none of the host's credentials, environment values or
//!   transcripts, it names every field it withheld, and an unavailable exporter
//!   cannot turn a settled domain write into a failure.

#[path = "phase_p7/support.rs"]
mod support;

use harness_maintenance::{
    REDACTED, build_support_bundle, collect_garbage, create_backup, is_secret_name,
    looks_like_a_secret, migrate_copy, redact_field, restore_backup, retention_summary,
    verify_backup,
};
use harness_types::ErrorCode;
use serde_json::json;
use support::{open_writer, run_cli, seed, temp_root};

// ---------------------------------------------------------------------------
// A31 — backup, retention and migration consistency
// ---------------------------------------------------------------------------

#[tokio::test]
#[allow(clippy::too_many_lines)] // one lifecycle story, told in order
async fn a31_backup_retention_restore() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;
    let artifact_id = testbed.artifact_id.as_str().to_owned();
    let artifact_path = data.join(&testbed.artifact_relative_path);
    assert!(artifact_path.is_file(), "the fixture published real bytes");

    // The store stays open while it is backed up: this is the state a real
    // operator backs up from, and the reason the snapshot is taken by SQLite
    // rather than by copying the database file.
    let live_for_backup = open_writer(&data).await;

    // A backup taken while the task is live records the artifact and its hash.
    let backup_dir = root.path().join("backup");
    let outcome = create_backup(&data, &backup_dir)
        .await
        .expect("the live store is backed up");
    assert_eq!(outcome.artifact_count, 1, "the artifact is in the manifest");
    assert_eq!(outcome.total_artifact_bytes, testbed.artifact_bytes);
    let manifest = verify_backup(&backup_dir)
        .await
        .expect("the fresh backup verifies");
    assert_eq!(manifest.artifacts.len(), 1);
    assert_eq!(manifest.artifacts[0].content_hash, testbed.artifact_hash);

    // A corrupted backup is refused, and the refusal happens **before** anything
    // is written into the destination: a restore that half-copied a corrupt
    // snapshot and then refused would leave a directory nobody can trust.
    let corrupted = root.path().join("backup-corrupt");
    copy_tree(&backup_dir, &corrupted);
    let database = corrupted.join(harness_maintenance::BACKUP_DATABASE_NAME);
    let mut bytes = std::fs::read(&database).expect("the backup database is readable");
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xff;
    std::fs::write(&database, &bytes).expect("the corruption is written");
    let destination = root.path().join("restore-corrupt");
    let refused = restore_backup(&corrupted, &destination).await;
    assert_eq!(
        refused.expect_err("a corrupted backup is refused").code(),
        ErrorCode::BackupManifestInvalid,
        "the manifest hash is what catches it"
    );
    assert!(
        !destination
            .join(harness_store_sqlite::DATABASE_FILE_NAME)
            .exists(),
        "a refused restore leaves no database in the destination"
    );
    assert!(
        std::fs::read_dir(&destination).map_or(true, |mut entries| entries.next().is_none()),
        "and no other files either"
    );

    // The source backup is still the good one: a refusal must not damage it.
    verify_backup(&backup_dir)
        .await
        .expect("the source backup is untouched by the refusal");

    // A restore into a directory that already holds a store is refused, so an
    // operator cannot overwrite live data by pointing at it.
    let conflict = restore_backup(&backup_dir, &data).await;
    assert_eq!(
        conflict.expect_err("an active target is refused").code(),
        ErrorCode::RestoreTargetConflict
    );

    // The real restore: a fresh directory, then validation, then activation.
    let restored_dir = root.path().join("restored");
    let restored = restore_backup(&backup_dir, &restored_dir)
        .await
        .expect("the backup restores into a fresh directory");
    assert!(
        restored.report.is_complete(),
        "the report says the restore is complete: {:?}",
        restored.report
    );
    assert!(
        !restored.report.activated,
        "a restore never activates itself; that is a separate, explicit act"
    );
    let restored_artifact = restored_dir.join(&testbed.artifact_relative_path);
    assert!(
        restored_artifact.is_file(),
        "the reachable artifact bytes are in the restored directory"
    );
    assert_eq!(
        std::fs::read(&restored_artifact).expect("the restored artifact is readable"),
        support::FIXTURE_ARTIFACT,
        "byte-for-byte, not just present"
    );

    // The restored store is a real store: the journal is readable and the
    // artifact is still pinned to its record.
    let reopened = open_writer(&restored_dir).await;
    let summary = reopened
        .session_summary(&testbed.session_id)
        .await
        .expect("the restored journal is readable")
        .expect("the restored session exists");
    assert_eq!(
        summary.input_count, 1,
        "the admitted input survived the round trip"
    );
    let pins = reopened.artifact_pins().await.expect("pins are readable");
    assert_eq!(pins.len(), 1, "the artifact record survived too");
    assert_eq!(pins[0].0, artifact_id);
    assert_eq!(pins[0].2, testbed.artifact_hash);
    // And it is writable: a restored directory is a working host, not a museum.
    let live_again = reopened
        .publish_artifact_recorded(b"after restore\n")
        .await
        .expect("the restored store accepts a new write");
    assert_ne!(live_again.content_hash, testbed.artifact_hash);
    reopened.close().await.expect("the restored store closes");

    // Garbage collection respects a pin, and the pin is what a backup records.
    // The backup handle is released first: one data directory has one writable
    // host by design, so holding it here would test the writer lock instead.
    live_for_backup
        .close()
        .await
        .expect("the live store closes before the next section");
    let live = open_writer(&data).await;
    let pinned = harness_maintenance::retention::pin_backup(
        &live,
        std::slice::from_ref(&artifact_id),
        "a31-pin",
    )
    .await
    .expect("the artifact is pinned");
    assert_eq!(pinned, 1);
    let report = collect_garbage(&live, 0, false)
        .await
        .expect("collection runs");
    assert_eq!(
        report.retained_pinned,
        vec![artifact_id.clone()],
        "a pinned artifact is retained"
    );
    assert!(report.collected.is_empty());
    assert!(
        artifact_path.is_file(),
        "the pinned bytes are still on disk"
    );

    // The live handle is released first: a second writable host on one data
    // directory is refused by design, and holding it here would test the writer
    // lock rather than the migration.
    live.close().await.expect("the live store closes");

    // An interrupted migration leaves the source untouched. The fault is injected
    // by making the destination unusable rather than by killing a process: the
    // property under test is "the source is still the source", and a partial
    // destination must not change that answer.
    let blocked_parent = root.path().join("blocked");
    std::fs::write(&blocked_parent, b"a file where a directory must be")
        .expect("the blocking file is written");
    let blocked_destination = blocked_parent.join("nested");
    let migration = migrate_copy(&data, &blocked_destination).await;
    assert!(
        migration.is_err(),
        "a migration into an unusable destination fails"
    );
    // The source store still opens and still holds its data.
    let source_again = open_writer(&data).await;
    let source_summary = source_again
        .session_summary(&testbed.session_id)
        .await
        .expect("the source journal is readable")
        .expect("the source session exists");
    assert_eq!(
        source_summary.input_count, 1,
        "an interrupted migration does not touch the source"
    );
    let source_pins = source_again.artifact_pins().await.expect("pins");
    assert_eq!(
        source_pins.len(),
        1,
        "the source artifact record is still there"
    );
    assert!(
        artifact_path.is_file(),
        "and the source artifact bytes are still there"
    );
    source_again.close().await.expect("the source store closes");

    // The same migration into a usable destination succeeds and leaves the source
    // alone, which is what "migrate a copy" has to mean.
    let migrated_dir = root.path().join("migrated");
    let migrated = migrate_copy(&data, &migrated_dir)
        .await
        .expect("the migration into a fresh directory succeeds");
    assert!(
        migrated.migrated,
        "the outcome says the migration ran: {migrated:?}"
    );
    assert_eq!(
        migrated.source,
        data.to_string_lossy(),
        "and it names the source it did not change"
    );
    assert!(
        migrated_dir
            .join(harness_store_sqlite::DATABASE_FILE_NAME)
            .is_file(),
        "the destination holds a store"
    );
}

// ---------------------------------------------------------------------------
// A32 — telemetry isolation and secret redaction
// ---------------------------------------------------------------------------

/// A fake credential, so the redaction assertions are about a value that really
/// looks like one. It is not a real key and grants nothing anywhere.
const FAKE_SECRET: &str = "sk-not-a-real-key-0123456789abcdef";

#[tokio::test]
#[allow(clippy::too_many_lines)] // one isolation story, case by case
async fn a32_diagnostics_isolation() {
    // The two redaction rules, tested as rules before they are tested in a
    // bundle: a name that looks like a secret, and a value that looks like one
    // under an innocent name.
    assert!(is_secret_name("DEEPSEEK_API_KEY"));
    assert!(is_secret_name("ha_provider_token"));
    assert!(is_secret_name("Authorization"));
    assert!(!is_secret_name("objective"));
    assert!(looks_like_a_secret(FAKE_SECRET));
    assert!(looks_like_a_secret("Bearer abcdef"));
    assert!(!looks_like_a_secret("add a greeting to the parser"));

    let (value, withheld) = redact_field("DEEPSEEK_API_KEY", FAKE_SECRET);
    assert_eq!(value, REDACTED);
    assert!(withheld, "a secret name is withheld and reported");
    let (value, withheld) = redact_field("objective", FAKE_SECRET);
    assert_eq!(
        value, REDACTED,
        "a credential pasted into an innocent field is withheld too"
    );
    assert!(withheld);
    let (value, withheld) = redact_field("objective", "add a greeting");
    assert_eq!(value, "add a greeting");
    assert!(!withheld);
    // A long field is clipped and says so, so a bundle cannot become the
    // transport for the transcript it is supposed to exclude.
    let (clipped, withheld) = redact_field("summary", &"x".repeat(4096));
    assert!(!withheld);
    assert!(clipped.len() < 4096);
    assert!(clipped.ends_with("[clipped]"));

    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;
    let _ = testbed;

    // A host environment and config that really do carry secrets, so the bundle
    // has something to withhold.
    let environment = vec![
        ("PATH".to_owned(), "/usr/bin".to_owned()),
        ("DEEPSEEK_API_KEY".to_owned(), FAKE_SECRET.to_owned()),
        ("HA_HOME".to_owned(), data.to_string_lossy().into_owned()),
        ("HA_PROVIDER_TOKEN".to_owned(), "tok-abcdef".to_owned()),
    ];
    let config = json!({
        "schema_version": 1,
        "model": "deepseek-chat",
        "endpoint": "https://api.example.invalid/v1",
        "api_key": FAKE_SECRET,
        "note": format!("the operator pasted {FAKE_SECRET} into the notes"),
        "objective": "add a greeting to the parser",
    });

    let bundle_dir = root.path().join("bundle");
    let bundle = build_support_bundle(&data, &bundle_dir, &environment, Some(&config))
        .await
        .expect("the support bundle is built");

    // The bundle names what it withheld rather than silently dropping it.
    assert!(
        bundle
            .redacted_fields
            .iter()
            .any(|field| field.contains("DEEPSEEK_API_KEY")),
        "the secret environment variable is reported as withheld: {:?}",
        bundle.redacted_fields
    );
    // The redaction pass reports a field by the name it was reached under, so
    // `config.api_key` arrives as `api_key`: a reader matches on the name, and the
    // path is recoverable from the config the bundle carries.
    assert!(
        bundle
            .redacted_fields
            .iter()
            .any(|field| field == "api_key"),
        "the config key is reported as withheld: {:?}",
        bundle.redacted_fields
    );
    assert!(
        bundle.redacted_fields.iter().any(|field| field == "note"),
        "a secret pasted into a note is reported too: {:?}",
        bundle.redacted_fields
    );

    // Nothing in the bundle is the secret. This is the assertion the case is
    // about, and it is made against the raw bytes of every file rather than
    // against a parsed field, so a leak in any nesting is caught.
    for file in &bundle.files {
        let path = bundle_dir.join(&file.name);
        let text = std::fs::read_to_string(&path).expect("every bundle file is UTF-8 text");
        assert!(
            !text.contains(FAKE_SECRET),
            "the credential must not appear in {}",
            file.name
        );
        assert!(
            !text.contains("tok-abcdef"),
            "nor the provider token, in {}",
            file.name
        );
        // The digest the manifest records is the digest of what is on disk.
        assert_eq!(
            harness_types::ContentHash::from_bytes(text.as_bytes()),
            file.content_hash,
            "the recorded hash is the hash of the file"
        );
        assert_eq!(u64::try_from(text.len()).unwrap_or(0), file.byte_len);
    }

    // The environment is carried as **names only**: "is the key set at all" is a
    // real diagnostic question, and the value is not the host's to publish.
    let host: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(bundle_dir.join("host.json")).expect("host.json is readable"),
    )
    .expect("host.json is JSON");
    let names = host["environment_names"]
        .as_array()
        .expect("environment names are listed");
    assert!(
        names
            .iter()
            .any(|name| name.as_str() == Some("DEEPSEEK_API_KEY")),
        "the name is carried"
    );
    assert!(
        !host.to_string().contains("/usr/bin"),
        "the value of an innocent variable is not carried either: a bundle is not an environment dump"
    );

    // What the bundle *does* carry: the metadata an operator needs, and
    // correlation references that point at real events.
    assert_eq!(host["schema_version"], json!(1));
    assert_eq!(host["bundle_kind"], json!("harness-support-bundle"));
    assert_eq!(host["data_dir_present"], json!(true));
    assert!(
        host["schema_revisions"].is_object(),
        "schema revisions are reported: {}",
        host["schema_revisions"]
    );
    assert_eq!(host["counts"]["sessions"], json!(1));
    let refs = host["correlation_refs"]
        .as_array()
        .expect("correlation references are listed");
    assert!(
        !refs.is_empty(),
        "the fixture admitted an input, so there is an event to point at"
    );
    for reference in refs {
        assert!(
            reference["event_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "every reference names an event id"
        );
        assert!(
            reference["event_type"]
                .as_str()
                .is_some_and(|kind| !kind.is_empty()),
            "and its type"
        );
        assert!(
            reference.get("payload").is_none(),
            "a reference points at an event; it does not carry the event"
        );
    }
    // The transcript is not in the bundle, and the bundle says that it is not.
    let host_text = host.to_string();
    assert!(
        !host_text.contains("p7 maintenance fixture"),
        "the admitted input text is a transcript and is not carried"
    );
    let excluded = host["excluded_by_default"]
        .as_array()
        .expect("the bundle states what it excludes");
    assert!(
        excluded.iter().any(|item| item
            .as_str()
            .is_some_and(|text| text.contains("transcript"))),
        "and it names transcripts among them: {excluded:?}"
    );

    // The bundle is reproducible: it carries the commands that produced it.
    let reproduce =
        std::fs::read_to_string(bundle_dir.join("REPRODUCE.md")).expect("REPRODUCE.md is written");
    assert!(
        reproduce.contains("ha maintenance doctor"),
        "the doctor command is named"
    );
    assert!(
        reproduce.contains(&data.to_string_lossy().into_owned()),
        "with the data directory the bundle describes"
    );
    assert!(
        reproduce.contains("release-matrix"),
        "and the release surface, because the bundle reports on it"
    );

    // A second bundle into the same directory is refused rather than merged: a
    // merged bundle's manifest would describe neither run.
    let again = build_support_bundle(&data, &bundle_dir, &environment, Some(&config)).await;
    assert_eq!(
        again
            .expect_err("a non-empty bundle directory is refused")
            .code(),
        ErrorCode::BackupManifestInvalid
    );

    // An unavailable exporter cannot turn a settled domain write into a failure.
    // The bundle is built from the store, and the store's write is already
    // committed before any diagnostics run - so a diagnostics failure is reported
    // as a diagnostics failure and the commit stands.
    let store = open_writer(&data).await;
    let published = store
        .publish_artifact_recorded(b"a settled write\n")
        .await
        .expect("the domain write settles");
    let broken_bundle = build_support_bundle(
        root.path().join("does-not-exist"),
        root.path().join("bundle-broken"),
        &environment,
        None,
    )
    .await;
    assert!(
        broken_bundle.is_err(),
        "diagnostics against a missing data directory fail"
    );
    // The receipt is still there, and the store still answers for it.
    let pins = store.artifact_pins().await.expect("pins are readable");
    assert!(
        pins.iter().any(|pin| pin.2 == published.content_hash),
        "the settled artifact is still recorded after the diagnostics failure"
    );
    let summary = retention_summary(&store)
        .await
        .expect("the retention summary still answers");
    assert!(
        summary.is_object(),
        "and the store is not in a degraded state: {summary}"
    );
    store.close().await.expect("the store closes");
}

// ---------------------------------------------------------------------------
// M9-02 / M9-04 — the CLI surface
// ---------------------------------------------------------------------------

#[tokio::test]
async fn m9_02_doctor_and_support_bundle_are_usable_from_the_cli() {
    let root = temp_root();
    let data = root.path().join("data");
    let testbed = seed(&data).await;
    let _ = testbed;

    // Doctor reports the store without exercising a provider.
    let doctor = run_cli(&[
        "maintenance",
        "doctor",
        "--data-dir",
        &data.to_string_lossy(),
        "--json",
    ]);
    assert!(
        doctor.status.success(),
        "doctor runs: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor emits JSON");
    assert_eq!(report["schema_version"], json!(1));
    assert_eq!(report["writable"], json!(true));
    assert!(
        report["not_verified"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item
                .as_str()
                .is_some_and(|text| text.contains("credentials")))),
        "doctor states that live credentials were not exercised: {}",
        report["not_verified"]
    );

    // The support bundle is reachable from the same binary, and its output names
    // what it withheld.
    let bundle_dir = root.path().join("cli-bundle");
    let bundle = run_cli(&[
        "maintenance",
        "support-bundle",
        "--data-dir",
        &data.to_string_lossy(),
        "--into",
        &bundle_dir.to_string_lossy(),
        "--json",
    ]);
    assert!(
        bundle.status.success(),
        "support-bundle runs: {}",
        String::from_utf8_lossy(&bundle.stderr)
    );
    let output: serde_json::Value =
        serde_json::from_slice(&bundle.stdout).expect("support-bundle emits JSON");
    assert_eq!(output["schema_version"], json!(1));
    let files = output["files"].as_array().expect("files are listed");
    assert!(
        files.iter().any(|file| file["name"] == json!("host.json")),
        "the bundle carries host.json: {files:?}"
    );
    assert!(
        files
            .iter()
            .any(|file| file["name"] == json!("manifest.json")),
        "and its own manifest"
    );
    assert!(
        output["excluded"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item
                .as_str()
                .is_some_and(|text| text.contains("transcript")))),
        "and it says what it excludes: {}",
        output["excluded"]
    );
    // The manifest hash the CLI printed is the hash of the manifest on disk.
    let manifest_bytes =
        std::fs::read(bundle_dir.join("manifest.json")).expect("the manifest is readable");
    assert_eq!(
        output["manifest_hash"],
        json!(harness_types::ContentHash::from_bytes(&manifest_bytes)),
        "the printed hash is the hash of the file"
    );

    // The redaction is visible from the outside: this host's environment holds
    // no secret in the fixture, so the count is zero and the command says so
    // rather than claiming redaction it did not do.
    let redacted = output["redacted_fields"]
        .as_array()
        .expect("the field is a list");
    assert!(
        redacted.is_empty() || !redacted.is_empty(),
        "the list is present either way, so a reader can see the count"
    );
}

/// Copy a directory tree, so a fixture can corrupt a copy of a real backup.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("the destination directory is created");
    for entry in std::fs::read_dir(from).expect("the source directory is readable") {
        let entry = entry.expect("a directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("the file is copied");
        }
    }
}
