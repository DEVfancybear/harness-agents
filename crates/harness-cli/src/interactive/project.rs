//! Durable project identity for one workspace root.
//!
//! A project id is generated, never derived, so it has to be *remembered*: the first
//! turn registers the root, and every later turn — including one in a new process —
//! resolves the id that was registered instead of inventing another. Without this,
//! every project-scoped record a turn writes (artifacts, approvals, memory) belongs to
//! an identity the next turn can never name again.

use std::path::Path;

use harness_store_sqlite::{SqliteStore, StoreError};
use harness_types::{HarnessError, ProjectId};

/// Resolve the project identity this workspace root is registered under.
///
/// The root is registered on first use. A root already bound to another identity is a
/// conflict the store reports rather than one this resolver papers over.
pub async fn resolve_project_id(
    store: &SqliteStore,
    root: &Path,
) -> Result<ProjectId, HarnessError> {
    let candidate = ProjectId::generate();
    let registration = harness_tools::workspace_registration(candidate.clone(), root)?;
    if let Some(existing) = store
        .registered_project(&registration.canonical_root)
        .await
        .map_err(StoreError::into_harness_error)?
    {
        return Ok(existing);
    }
    store
        .register_project(registration)
        .await
        .map_err(StoreError::into_harness_error)?;
    Ok(candidate)
}

#[cfg(test)]
mod tests {
    use super::resolve_project_id;
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_types::{HostId, ProjectId};
    use std::sync::Arc;

    /// The promise the whole feature rests on: the same root resolves to one identity.
    #[tokio::test]
    async fn a_workspace_root_keeps_one_project_identity_across_processes() {
        let temp = tempfile::tempdir().expect("temp root");
        let workspace = temp.path().join("project");
        std::fs::create_dir(&workspace).expect("workspace root");
        let data = temp.path().join("data");

        let first = {
            let store = Arc::new(
                SqliteStore::open_writer(WriterOpenOptions::new(data.clone(), HostId::generate()))
                    .await
                    .expect("store opens"),
            );
            let id = resolve_project_id(&store, &workspace)
                .await
                .expect("first resolution registers");
            Arc::try_unwrap(store)
                .expect("single owner")
                .close()
                .await
                .expect("store closes");
            id
        };

        // A second process opens the same store and must find the same id.
        let store =
            SqliteStore::open_writer(WriterOpenOptions::new(data.clone(), HostId::generate()))
                .await
                .expect("store reopens");
        assert_eq!(
            resolve_project_id(&store, &workspace)
                .await
                .expect("second resolution reads"),
            first,
            "a new run must not invent a new project identity"
        );
        store.close().await.expect("store closes");
    }

    /// A different root is a different project, even in the same data directory.
    #[tokio::test]
    async fn another_root_is_another_project() {
        let temp = tempfile::tempdir().expect("temp root");
        let one = temp.path().join("one");
        let two = temp.path().join("two");
        std::fs::create_dir(&one).expect("first root");
        std::fs::create_dir(&two).expect("second root");
        let store = SqliteStore::open_writer(WriterOpenOptions::new(
            temp.path().join("data"),
            HostId::generate(),
        ))
        .await
        .expect("store opens");
        let first = resolve_project_id(&store, &one).await.expect("first root");
        let second = resolve_project_id(&store, &two).await.expect("second root");
        assert_ne!(first, second);
        assert!(ProjectId::parse(first.as_str()).is_ok());
        store.close().await.expect("store closes");
    }
}
