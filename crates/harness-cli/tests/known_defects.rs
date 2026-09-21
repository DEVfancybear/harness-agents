//! D1-repro: a lock inside the workspace stops a turn before it starts.
//!
//! `observe_workspace` hashes every file it walks, and Windows answers a read of a file
//! whose byte range another process holds with `os error 33` (`ERROR_LOCK_VIOLATION`). A
//! `SQLite` store holds exactly such locks for as long as a writer connection is open.
//!
//! Measured on this tree (`cargo test -p harness-cli --test known_defects -- --ignored`):
//! with a live `SqliteStore` under `<workspace>/data/projects/<key>/`, every turn fails with
//!
//! ```text
//! workspace_escape: cannot hash workspace file: The process cannot access the file because
//! another process has locked a portion of the file. (os error 33)
//! ```
//!
//! Two things are wrong and neither is fixed here, because both are their own change:
//!
//! 1. **The error is misdiagnosed.** A lock violation is not a workspace escape, and the code
//!    sends an operator looking for a path-escape bug that does not exist.
//! 2. **A locked file is fatal, not skipped.** The walk already skips sensitive paths; a store
//!    the app itself owns is the same kind of "not workspace content".
//!
//! The default layout does **not** hit this: on Windows the store lives in
//! `%LOCALAPPDATA%\HarnessAgents\data`, outside the project. It is reachable through the
//! documented `HA_HOME` knob when that points inside the project the app is started in —
//! which is exactly what a first run in such a layout does *not* show, because the store does
//! not exist yet: turn one succeeds, and every turn after it fails.
//!
//! Ignored so the suite stays green; run it by hand to reproduce the measurement above.

use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::observe_workspace;
use harness_types::{HostId, ProjectId};

#[tokio::test]
#[ignore = "documents a known defect (workspace hash vs. locked store); run by hand"]
async fn a_live_store_inside_the_workspace_breaks_the_fingerprint() {
    let temp = tempfile::tempdir().expect("temp root");
    let project = temp.path().join("project");
    let store_dir = project
        .join("data")
        .join("projects")
        .join("project-deadbeef");
    std::fs::create_dir_all(&store_dir).expect("store dir");
    let store = SqliteStore::open_writer(WriterOpenOptions::new(
        store_dir.clone(),
        HostId::generate(),
    ))
    .await
    .expect("store opens");
    std::fs::write(project.join("build output.log"), "hello\n").expect("fixture log");

    let outcome = observe_workspace(ProjectId::generate(), &project);
    match outcome {
        Ok(observation) => println!(
            "observed {:?}: the defect is gone, close this test",
            observation.observed_fingerprint
        ),
        Err(error) => println!(
            "still broken: {} {} (locked files present: {})",
            error.code(),
            error,
            store_dir.join("harness.sqlite3-shm").exists()
        ),
    }
    let _ = store.close().await;
}
