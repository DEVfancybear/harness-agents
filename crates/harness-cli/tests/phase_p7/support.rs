//! Shared fixtures for the P7 acceptance target.
//!
//! Every fixture here is real: a real `SQLite` store opened through the ordinary
//! writer path, real session admission through the P2 service, real artifact
//! bytes on disk, and the real compiled `ha` binary for the packaged-artifact
//! checks. Only external boundaries are absent — no live provider credential and
//! no remote backup target exist, and neither is claimed.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use harness_maintenance::MaintenanceError;
use harness_session::{AdmitInputRequest, SessionService};
use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_types::{
    ArtifactId, ContentHash, ErrorCode, HostId, InputId, ProjectId, SessionId, SourceAuthority,
    TaskId, WorkspaceObservation,
};
use serde_json::Value;

/// The artifact body every fixture store publishes. Exactly 20 bytes, so a byte
/// count in a report can be checked rather than assumed.
pub const FIXTURE_ARTIFACT: &[u8] = b"p7 fixture artifact\n";

/// A real data directory with one session and one published artifact.
pub struct TestBed {
    pub data_dir: PathBuf,
    pub session_id: SessionId,
    pub artifact_id: ArtifactId,
    pub artifact_hash: ContentHash,
    pub artifact_relative_path: String,
    pub artifact_bytes: u64,
}

/// A disposable parent directory. The store never lives inside the repository.
#[must_use]
pub fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Open a writable store at a data directory through the ordinary writer path.
pub async fn open_writer(data_dir: impl AsRef<Path>) -> SqliteStore {
    SqliteStore::open_writer(WriterOpenOptions::new(
        data_dir.as_ref(),
        HostId::generate(),
    ))
    .await
    .expect("store opens writable")
}

/// A data directory holding one admitted session and one published artifact.
///
/// The store is returned open and writable, which is the state a real operator
/// would back up from.
pub async fn fixture_store(data_dir: impl AsRef<Path>) -> (SqliteStore, ArtifactId) {
    let testbed = seed(data_dir).await;
    let store = open_writer(&testbed.data_dir).await;
    (store, testbed.artifact_id)
}

/// The same fixture, returning every identity it created.
pub async fn seed(data_dir: impl AsRef<Path>) -> TestBed {
    let data_dir = data_dir.as_ref().to_path_buf();
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let store = std::sync::Arc::new(open_writer(&data_dir).await);
    let session_id = SessionId::generate();
    SessionService::new(std::sync::Arc::clone(&store))
        .admit_input(AdmitInputRequest {
            session_id: session_id.clone(),
            task_id: TaskId::generate(),
            input_id: InputId::generate(),
            expected_sequence: 1,
            authority: SourceAuthority::User,
            raw_text: "p7 maintenance fixture".to_owned(),
            workspace: WorkspaceObservation {
                project_id: ProjectId::generate(),
                worktree_id: "p7-fixture".to_owned(),
                base_commit: "0".repeat(40),
                observed_fingerprint: ContentHash::from_bytes(b"p7-fixture"),
            },
            initial_plan_items: Vec::new(),
        })
        .await
        .expect("admit fixture input");
    let artifact = store
        .publish_artifact_recorded(FIXTURE_ARTIFACT)
        .await
        .expect("publish artifact");
    let bytes = u64::try_from(FIXTURE_ARTIFACT.len()).expect("byte length");
    std::sync::Arc::try_unwrap(store)
        .expect("fixture store released")
        .close()
        .await
        .expect("store closes");
    TestBed {
        data_dir,
        session_id,
        artifact_id: artifact.artifact_id,
        artifact_hash: artifact.content_hash,
        artifact_relative_path: artifact.relative_path,
        artifact_bytes: bytes,
    }
}

/// Run the compiled `ha` binary from the packaged artifact.
#[must_use]
pub fn run_cli(arguments: &[&str]) -> Output {
    Command::new(cli_binary())
        .args(arguments)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("ha binary runs")
}

/// Resolve the compiled `ha` executable.
///
/// The binary belongs to this crate, so the compile-time variable names the real
/// packaged artifact rather than a path guess.
#[must_use]
pub fn cli_binary() -> PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_ha") {
        let candidate = PathBuf::from(path);
        assert!(
            candidate.is_file(),
            "compiled ha binary missing at {}",
            candidate.display()
        );
        return candidate;
    }
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let candidate = path.join(format!("ha{}", std::env::consts::EXE_SUFFIX));
    assert!(
        candidate.is_file(),
        "compiled ha binary missing at {}",
        candidate.display()
    );
    candidate
}

/// The workspace root, for documentation and registry checks.
///
/// The path deliberately keeps its `..` components: on Windows, canonicalising a
/// directory path yields a `\\?\` verbatim path, so the plain relative form is
/// what the real filesystem checks below expect.
#[must_use]
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every acceptance case this release claims to exercise: C01-C30 and K01-K14.
#[must_use]
pub fn all_case_ids() -> Vec<String> {
    let mut cases = (1..=30)
        .map(|index| format!("C{index:02}"))
        .collect::<Vec<_>>();
    cases.extend((1..=14).map(|index| format!("K{index:02}")));
    cases
}

/// The acceptance registry rows, read from the real registry file.
#[must_use]
pub fn registry_cases() -> Vec<Value> {
    let path = repository_root().join("tests/acceptance/registry.json");
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("registry is readable at {}: {error}", path.display()));
    let registry: Value = serde_json::from_slice(&bytes).expect("registry is valid JSON");
    registry["cases"]
        .as_array()
        .expect("registry has a cases array")
        .clone()
}

/// Assert one maintenance result carries an expected error code.
pub fn assert_code(result: Result<(), MaintenanceError>, code: ErrorCode) {
    match result {
        Ok(()) => panic!("expected {code:?}, got success"),
        Err(error) => assert_eq!(error.code(), code, "unexpected error: {error}"),
    }
}
