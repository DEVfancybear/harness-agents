//! Durable project identity for one workspace root.
//!
//! A project id is generated, never derived, so it has to be *remembered*: the first
//! turn registers the root, and every later turn — including one in a new process —
//! resolves the id that was registered instead of inventing another. Without this,
//! every project-scoped record a turn writes (artifacts, approvals, memory) belongs to
//! an identity the next turn can never name again.

use std::path::Path;

use harness_store_sqlite::{SqliteStore, StoreError};
use harness_types::{ErrorCode, HarnessError, ProjectId};

/// Resolve the project identity this workspace root is registered under.
///
/// The root is registered on first use. A directory of a repository that is
/// not its key root ([`super::bootstrap::project_root`]: a subdirectory, a
/// linked worktree) shares the key root's identity: the key root is
/// registered first when it is not yet, and the directory is registered
/// under the same id. When the store refuses that (the two do not share a
/// Git common directory it can see), the directory gets an identity of its
/// own, as every directory did before. A root already bound to another
/// identity is a conflict the store reports rather than one this resolver
/// papers over.
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
    if let Some(id) = repository_identity(store, &super::bootstrap::project_root(root)).await {
        let shared = harness_tools::workspace_registration(id.clone(), root)?;
        // The store cannot tie the two together (a conflict): the directory
        // is a project of its own, as before. A read-only store cannot
        // register the directory, and still names the project's identity.
        let tied = store.register_project(shared).await;
        if !tied.is_err_and(|error| error.code() == ErrorCode::ProjectIdentityConflict) {
            return Ok(id);
        }
    }
    store
        .register_project(registration)
        .await
        .map_err(StoreError::into_harness_error)?;
    Ok(candidate)
}

/// The identity another directory of the project keyed by `key_root` is
/// registered under in this store: the key root's own first, then any other.
async fn repository_identity(store: &SqliteStore, key_root: &Path) -> Option<ProjectId> {
    let roots = store.registered_roots().await.ok()?;
    let mut ours = roots.into_iter().filter(|(registered, _)| {
        super::bootstrap::project_root(Path::new(registered)) == key_root
    });
    let first = ours.next()?;
    Some(
        std::iter::once(first.clone())
            .chain(ours)
            .find(|(registered, _)| {
                super::bootstrap::canonical_or_given(Path::new(registered)) == key_root
            })
            .unwrap_or(first)
            .1,
    )
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

    /// A subdirectory and a linked worktree of a repository are the
    /// repository's project, whichever directory ha ran in first; a store
    /// adopted from a subdirectory gives its identity to the root.
    #[tokio::test]
    async fn the_directories_of_one_repository_share_its_identity() {
        let temp = tempfile::tempdir().expect("temp root");
        let repo = temp.path().join("repo");
        let sub = repo.join("crates").join("app");
        std::fs::create_dir_all(&sub).expect("subdirectory");
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&repo)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("git runs");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(repo.join("README.md"), "repo\n").expect("file");
        git(&["add", "README.md"]);
        git(&["commit", "-q", "-m", "first"]);
        let tree = temp.path().join("tree");
        git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            &tree.to_string_lossy(),
            "HEAD",
        ]);

        let open = |name: &str| {
            SqliteStore::open_writer(WriterOpenOptions::new(
                temp.path().join(name),
                HostId::generate(),
            ))
        };
        // The subdirectory first, as a store adopted from it was registered.
        let store = open("one").await.expect("store opens");
        let from_sub = resolve_project_id(&store, &sub)
            .await
            .expect("subdirectory");
        let from_root = resolve_project_id(&store, &repo).await.expect("root");
        let from_tree = resolve_project_id(&store, &tree).await.expect("worktree");
        assert_eq!(from_root, from_sub);
        assert_eq!(from_tree, from_sub);
        assert_eq!(
            resolve_project_id(&store, &sub).await.expect("again"),
            from_sub
        );
        store.close().await.expect("store closes");

        // The root first.
        let store = open("two").await.expect("store opens");
        let from_root = resolve_project_id(&store, &repo).await.expect("root");
        assert_eq!(
            resolve_project_id(&store, &sub)
                .await
                .expect("subdirectory"),
            from_root
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
