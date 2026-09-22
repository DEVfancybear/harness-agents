//! D1 regression: a lock inside the workspace no longer stops a turn.
//!
//! `observe_workspace` hashes every file it walks, and Windows answers a read of
//! a file whose byte range another process holds with `os error 33`
//! (`ERROR_LOCK_VIOLATION`). A `SQLite` store holds exactly such locks for as
//! long as a writer connection is open.
//!
//! The original defect, measured on this tree, made every turn fail with
//!
//! ```text
//! workspace_escape: cannot hash workspace file: The process cannot access the file because
//! another process has locked a portion of the file. (os error 33)
//! ```
//!
//! Two things were wrong:
//!
//! 1. **The error was misdiagnosed.** A lock violation is not a workspace
//!    escape, and the message sent an operator looking for a path-escape bug
//!    that did not exist. Read failures now report `storage_open_failed` and
//!    name the file.
//! 2. **A locked file was fatal.** The walk already skips sensitive paths; a
//!    file another process holds open is now recorded in the fingerprint as
//!    present-but-unreadable (`content_hash: null, unreadable: "locked"`)
//!    instead of failing the turn. If it later becomes readable, its content
//!    hash appears and the fingerprint changes, so an approval bound to the
//!    unreadable state is invalidated rather than silently comparing equal.
//!
//! The default layout does not hit this: on Windows the store lives in
//! `%LOCALAPPDATA%\HarnessAgents\data`, outside the project. It is reachable
//! through the documented `HA_HOME` knob when that points inside the project the
//! app is started in — which is exactly what a first run in such a layout does
//! *not* show, because the store does not exist yet: turn one succeeded, and
//! every turn after it failed.

use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
use harness_tools::observe_workspace;
use harness_types::{HostId, ProjectId};

#[tokio::test]
async fn a_live_store_inside_the_workspace_no_longer_breaks_the_fingerprint() {
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

    // A live writer holds `-shm`/`-wal` byte-range locks; the observation must
    // succeed anyway, and it must be a real fingerprint.
    let observation = observe_workspace(ProjectId::generate(), &project)
        .expect("a live store inside the workspace must not fail the fingerprint");
    assert!(
        observation
            .observed_fingerprint
            .as_str()
            .starts_with("sha256:")
    );
    assert!(
        store_dir.join("harness.sqlite3-shm").exists() || !cfg!(windows),
        "the fixture should have produced a live store"
    );
    let _ = store.close().await;
}
