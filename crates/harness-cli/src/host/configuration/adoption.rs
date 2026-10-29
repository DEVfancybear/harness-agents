//! A project's data kept under the key it had before keys followed the
//! repository (`docs/architecture/PROJECT_IDENTITY.en.md`).
//!
//! Before 0.8.8 a project was keyed by the directory ha started in, so a
//! launch from a repository's subdirectory or from a linked worktree kept a
//! store of its own. Now the key is the repository's: the first launch that
//! resolves the new key finds the store the old key named and moves it into
//! place, when nothing else holds it and the new key has no store yet. The
//! project memory and lessons kept under the old key are
//! given the new one. Two stores that both hold data are never merged: the
//! older one is left where it is and named in a notice, and `ha doctor`
//! lists it.
//!
//! Every step is safe to run twice or from two processes at once: the move
//! happens under one lock, each step looks again before it acts, and each
//! leaves nothing half done for the next launch to trip over.

use std::path::{Path, PathBuf};

use harness_store_sqlite::{DATABASE_FILE_NAME, WRITER_LOCK_FILE_NAME};
use serde::{Deserialize, Serialize};

/// What a project's store directory says about its key: the directory the
/// key names, and the older keys whose data it took.
pub const MANIFEST_FILE: &str = "project.json";
/// The lock an adoption holds, beside the projects' directories.
const LOCK_FILE: &str = "adoption.lock";
const SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    pub schema_version: u16,
    /// The directory the key names.
    pub root: String,
    /// Older keys whose store, memory and lessons this key took.
    #[serde(default)]
    pub adopted: Vec<String>,
    /// Older keys whose store held data while this key's did too: their
    /// memory and lessons were given this key, their store was left apart.
    #[serde(default)]
    pub apart: Vec<String>,
}

/// The manifest of a store directory, when it has one.
#[must_use]
pub fn manifest(store_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(store_dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_manifest(store_dir: &Path, manifest: &Manifest) -> Result<(), String> {
    let path = store_dir.join(MANIFEST_FILE);
    let staged = store_dir.join(format!("{MANIFEST_FILE}.staged"));
    let text = serde_json::to_string_pretty(manifest).map_err(|error| error.to_string())?;
    std::fs::write(&staged, text + "\n")
        .and_then(|()| std::fs::rename(&staged, &path))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn has_store(dir: &Path) -> bool {
    dir.join(DATABASE_FILE_NAME).is_file()
}

/// The notice a launch shows while an older store of the project is kept apart.
fn apart_notice(legacy_dir: &Path, store_dir: &Path) -> String {
    format!(
        "an older store of this project is kept apart at {} (its conversations are not in {}); nothing was merged",
        legacy_dir.display(),
        store_dir.display()
    )
}

/// Adopt what the project keyed `key` (its key root `root`) kept under
/// `legacy`, the key the launch directory had before. Returns the notices the
/// launch shows; nothing is returned or written when there is nothing to
/// adopt.
#[must_use]
pub fn adopt(data_dir: &Path, root: &Path, key: &str, legacy: &str) -> Vec<String> {
    let projects = data_dir.join("projects");
    let store_dir = projects.join(key);
    let legacy_dir = projects.join(legacy);
    // The common case costs two file checks: no store was ever kept under the
    // old key, or it was adopted and is gone.
    if !has_store(&legacy_dir) {
        return Vec::new();
    }
    if has_store(&store_dir)
        && manifest(&store_dir)
            .is_some_and(|manifest| manifest.apart.iter().any(|old| old == legacy))
    {
        return vec![apart_notice(&legacy_dir, &store_dir)];
    }
    let _lock = match lock(data_dir) {
        Ok(lock) => lock,
        Err(error) => {
            harness_types::diag::warn("project", "adoption_skipped")
                .msg(format!("the adoption lock could not be taken: {error}"))
                .field("legacy", legacy_dir.display().to_string())
                .emit();
            return Vec::new();
        }
    };
    // Looked at again under the lock: another process may have done it.
    if !has_store(&legacy_dir) {
        return Vec::new();
    }
    if has_store(&store_dir) {
        retag(data_dir, root, key, legacy, &legacy_dir, &store_dir);
        record(&store_dir, root, |manifest| &mut manifest.apart, legacy);
        harness_types::diag::warn("project", "store_kept_apart")
            .msg("two stores of one project both hold data; neither was merged")
            .field("store", store_dir.display().to_string())
            .field("legacy", legacy_dir.display().to_string())
            .emit();
        return vec![apart_notice(&legacy_dir, &store_dir)];
    }
    if let Some(holder) = held(data_dir, &legacy_dir) {
        harness_types::diag::info("project", "adoption_deferred")
            .msg(format!(
                "the older store is in use by {holder}; it moves on a later launch"
            ))
            .field("legacy", legacy_dir.display().to_string())
            .emit();
        return vec![format!(
            "the older store of this project at {} is in use by {holder}; it moves to {} when that ends",
            legacy_dir.display(),
            store_dir.display()
        )];
    }
    if let Err(error) = move_store(&legacy_dir, &store_dir) {
        harness_types::diag::warn("project", "adoption_failed")
            .msg(error.clone())
            .field("store", store_dir.display().to_string())
            .field("legacy", legacy_dir.display().to_string())
            .emit();
        return vec![format!(
            "the older store of this project at {} could not be moved to {}: {error}",
            legacy_dir.display(),
            store_dir.display()
        )];
    }
    retag(data_dir, root, key, legacy, &legacy_dir, &store_dir);
    record(&store_dir, root, |manifest| &mut manifest.adopted, legacy);
    harness_types::diag::info("project", "store_adopted")
        .msg("the store kept under the launch directory's older key moved to the repository's key")
        .field("store", store_dir.display().to_string())
        .field("legacy", legacy_dir.display().to_string())
        .emit();
    Vec::new()
}

/// Add `legacy` to one list of the store's manifest.
fn record(
    store_dir: &Path,
    root: &Path,
    list: impl FnOnce(&mut Manifest) -> &mut Vec<String>,
    legacy: &str,
) {
    let mut manifest = manifest(store_dir).unwrap_or_else(|| Manifest {
        schema_version: SCHEMA_VERSION,
        ..Manifest::default()
    });
    manifest.root = root.display().to_string();
    let keys = list(&mut manifest);
    if !keys.iter().any(|key| key == legacy) {
        keys.push(legacy.to_owned());
    }
    if let Err(error) = write_manifest(store_dir, &manifest) {
        harness_types::diag::warn("project", "manifest_unwritten")
            .msg(error)
            .emit();
    }
}

fn lock(data_dir: &Path) -> Result<std::fs::File, String> {
    super::files::hold(
        &data_dir.join(LOCK_FILE),
        std::time::Duration::from_secs(10),
        "adoption",
    )
}

/// What holds the store at `dir`, if anything does: a writer (its
/// `writer.lock`) or a background worker serving it (its registry lock).
fn held(data_dir: &Path, dir: &Path) -> Option<&'static str> {
    let busy = |path: &Path| {
        std::fs::File::options()
            .read(true)
            .write(true)
            .open(path)
            .is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
    };
    if busy(&dir.join(WRITER_LOCK_FILE_NAME)) {
        return Some("another ha process");
    }
    let workers = super::agents::registry::directory(data_dir);
    let worker = super::agents::registry::project_key(dir);
    busy(&super::agents::registry::lock_path(&workers, &worker)).then_some("a background worker")
}

/// Move the older store directory to the new key's place. A directory
/// already there without a store (made before any store was) is set aside
/// beside it rather than deleted.
fn move_store(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() && std::fs::remove_dir(to).is_err() {
        let aside = to.with_file_name(format!(
            "{}.before-adoption-{}",
            to.file_name().unwrap_or_default().to_string_lossy(),
            chrono::Utc::now().timestamp_millis()
        ));
        std::fs::rename(to, &aside)
            .map_err(|error| format!("{} could not be set aside: {error}", to.display()))?;
    }
    std::fs::rename(from, to).map_err(|error| error.to_string())
}

/// Give the project's memory, lessons and agent names the
/// new key, and drop the worker journal that would restart agents on the old
/// store. Each part is retried by the next launch that finds the old store,
/// and is a no-op once done.
fn retag(
    data_dir: &Path,
    root: &Path,
    key: &str,
    legacy: &str,
    legacy_dir: &Path,
    store_dir: &Path,
) {
    let tag = super::memory::ProjectTag::named(key.to_owned(), root);
    let warn = |what: &str, error: String| {
        harness_types::diag::warn("project", "retag_failed")
            .msg(format!("{what}: {error}"))
            .field("legacy", legacy.to_owned())
            .emit();
    };
    if let Err(error) = super::memory::retag_project(data_dir, legacy, &tag) {
        warn("project memory", error);
    }
    if let Err(error) = super::lessons::retag_project(
        &super::harness::global_dir(data_dir),
        legacy,
        key,
        &tag.name,
    ) {
        warn("lessons", error);
    }
    // A store left apart keeps its own worker files: its agents still run on it.
    if has_store(legacy_dir) {
        return;
    }
    let workers = super::agents::registry::directory(data_dir);
    let (old, new) = (
        super::agents::registry::project_key(legacy_dir),
        super::agents::registry::project_key(store_dir),
    );
    let _ = std::fs::remove_file(super::agents::registry::journal_path(&workers, &old));
    let names = |key: &str| workers.join(format!("{key}.names.json"));
    if names(&old).is_file() && std::fs::rename(names(&old), names(&new)).is_err() {
        let _ = std::fs::remove_file(names(&old));
    }
}

/// [`other_stores`] of the project ha runs in at `launch`, for `ha doctor`.
#[must_use]
pub fn other_stores_of(data_dir: &Path, launch: &Path) -> Vec<String> {
    other_stores(
        data_dir,
        &super::bootstrap::project_root(launch),
        &super::bootstrap::project_key_of(launch),
    )
    .iter()
    .map(|store| store.display().to_string())
    .collect()
}

/// The other stores under `data_dir` that hold this project: stores whose
/// registered roots key to `root` under another key. Each store's own
/// registrations are read; a store that cannot be read is left out.
#[must_use]
pub fn other_stores(data_dir: &Path, root: &Path, key: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("projects")) else {
        return Vec::new();
    };
    let mut found = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir.file_name().is_some_and(|name| name != key) && has_store(dir))
        .filter(|dir| {
            registered_roots(dir)
                .iter()
                .any(|registered| super::bootstrap::project_root(Path::new(registered)) == root)
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

fn registered_roots(dir: &Path) -> Vec<String> {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        dir.join(DATABASE_FILE_NAME),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return Vec::new();
    };
    let Ok(mut statement) = connection.prepare("SELECT canonical_root FROM project_registrations")
    else {
        return Vec::new();
    };
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_FILE, manifest, other_stores};
    use crate::interactive::agents::registry;
    use crate::interactive::bootstrap::{self, LaunchContext, LaunchRequest};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use crate::interactive::{harness, lessons, memory_store};
    use harness_store_sqlite::{SqliteStore, WriterOpenOptions};
    use harness_types::HostId;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    struct Project {
        temp: tempfile::TempDir,
        repo: PathBuf,
        sub: PathBuf,
    }

    impl Project {
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temp");
            let repo = temp.path().join("repo");
            let sub = repo.join("app");
            std::fs::create_dir_all(repo.join(".git")).expect("repository");
            std::fs::create_dir_all(&sub).expect("subdirectory");
            Self { temp, repo, sub }
        }

        fn data(&self) -> PathBuf {
            self.temp.path().join("home").join("data")
        }

        fn resolve(&self, directory: &Path) -> LaunchContext {
            bootstrap::resolve(LaunchRequest {
                cwd: Some(directory.to_path_buf()),
                caller_dir: self.temp.path().to_path_buf(),
                platform: HostPlatform::current(),
                environment: LaunchEnvironment::from_pairs([(
                    "HA_HOME",
                    self.temp.path().join("home").to_string_lossy().into_owned(),
                )]),
                explicit_data_dir: None,
            })
            .expect("context resolves")
        }

        /// The store a launch from `directory` made before keys followed the
        /// repository.
        fn legacy_dir(&self, directory: &Path) -> PathBuf {
            self.data()
                .join("projects")
                .join(bootstrap::legacy_key_of(directory))
        }
    }

    async fn store_at(directory: &Path) -> SqliteStore {
        SqliteStore::open_writer(WriterOpenOptions::new(
            directory.to_path_buf(),
            HostId::generate(),
        ))
        .await
        .expect("store opens")
    }

    async fn make_store(directory: &Path) {
        store_at(directory)
            .await
            .close()
            .await
            .expect("store closes");
    }

    /// One project entry in the global memory store and one lesson, kept for
    /// the project keyed `key`.
    fn keep_for(data: &Path, key: &str, root: &Path) {
        let global = harness::global_dir(data);
        let document = json!({
            "schema": 1,
            "entries": {"prompt": {}, "skill": {}, "subagent": {}, "memory": {
                "build-command": {
                    "id": "build-command", "title": "build", "version": 1,
                    "content": "the app builds with cargo build -p app",
                    "project": {"key": key, "name": "app"},
                },
                "editor": {"id": "editor", "title": "editor", "version": 1, "content": "the user edits in Zed"},
            }},
            "refinements": [],
        });
        memory_store::save_document(
            &global,
            &document,
            &memory_store::Writer::new("model", &json!({"via": "test"}), "create"),
        )
        .expect("memory saved");
        std::fs::write(
            global.join(lessons::FILE),
            json!({"lessons": [{
                "id": "les_1", "text": "run the app's tests", "kind": "correction",
                "project": {"key": key, "name": "app", "root": root.display().to_string()},
            }]})
            .to_string(),
        )
        .expect("lessons");
    }

    fn project_of(data: &Path, entry: &str) -> serde_json::Value {
        memory_store::load_document(&harness::global_dir(data)).expect("memory")["entries"]
            ["memory"][entry]["project"]
            .clone()
    }

    #[tokio::test]
    async fn a_store_a_subdirectory_kept_is_adopted_with_its_memory_and_lessons() {
        let project = Project::new();
        let data = project.data();
        let legacy_dir = project.legacy_dir(&project.sub);
        let legacy = bootstrap::legacy_key_of(&project.sub);
        make_store(&legacy_dir).await;
        keep_for(&data, &legacy, &project.sub);
        let ids = memory_store::ids(&harness::global_dir(&data)).expect("ids");
        let workers = registry::directory(&data);
        let old_worker = registry::project_key(&legacy_dir);
        std::fs::create_dir_all(&workers).expect("workers");
        std::fs::write(registry::journal_path(&workers, &old_worker), "{}").expect("journal");

        let context = project.resolve(&project.sub);
        let store_dir = context.project_store_dir();
        assert!(context.notices.is_empty(), "{:?}", context.notices);
        assert!(
            store_dir.join("harness.sqlite3").is_file(),
            "the store moved"
        );
        assert!(!legacy_dir.exists(), "nothing is left under the old key");
        let kept = manifest(&store_dir).expect("manifest");
        assert_eq!(kept.adopted, std::slice::from_ref(&legacy));
        assert_eq!(Path::new(&kept.root), context.project.key_root);
        // The memory is the repository's now, under the same ids.
        assert_eq!(
            project_of(&data, "build-command"),
            json!({"key": context.project.key, "name": "repo"})
        );
        assert_eq!(project_of(&data, "editor"), serde_json::Value::Null);
        assert_eq!(
            memory_store::ids(&harness::global_dir(&data)).expect("ids"),
            ids
        );
        let lessons = lessons::load(&harness::global_dir(&data)).expect("lessons");
        let about = lessons.lessons[0].project.as_ref().expect("project lesson");
        assert_eq!(about.key, context.project.key);
        assert_eq!(
            Path::new(&about.root),
            project.sub,
            "where it was seen stays"
        );
        assert!(!registry::journal_path(&workers, &old_worker).exists());
        // The moved store opens where the key says.
        let store = SqliteStore::open_read_only(store_dir.clone())
            .await
            .expect("the adopted store opens");
        store.close().await.expect("close");

        // A second launch, from the root or the subdirectory, has nothing to do.
        let again = project.resolve(&project.sub);
        assert!(again.notices.is_empty());
        assert_eq!(again.project_store_dir(), store_dir);
        assert_eq!(
            project.resolve(&project.repo).project_store_dir(),
            store_dir
        );
        assert_eq!(manifest(&store_dir).expect("manifest").adopted, [legacy]);
    }

    #[tokio::test]
    async fn two_stores_that_both_hold_data_are_left_apart_with_a_notice() {
        let project = Project::new();
        let data = project.data();
        let root_dir = project.legacy_dir(&project.repo);
        let legacy_dir = project.legacy_dir(&project.sub);
        let legacy = bootstrap::legacy_key_of(&project.sub);
        make_store(&root_dir).await;
        {
            // Registered as a launch from the subdirectory registered it.
            let store = store_at(&legacy_dir).await;
            crate::interactive::project::resolve_project_id(&store, &project.sub)
                .await
                .expect("registered");
            store.close().await.expect("close");
        }
        keep_for(&data, &legacy, &project.sub);

        let context = project.resolve(&project.sub);
        assert_eq!(context.project_store_dir(), root_dir);
        assert_eq!(context.notices.len(), 1, "{:?}", context.notices);
        assert!(
            context.notices[0].contains(&legacy_dir.display().to_string()),
            "the notice names the other store: {:?}",
            context.notices
        );
        assert!(
            context
                .header_lines()
                .iter()
                .any(|line| line.starts_with("Note:") && line.contains("kept apart")),
            "{:?}",
            context.header_lines()
        );
        assert!(legacy_dir.join("harness.sqlite3").is_file(), "not merged");
        assert!(root_dir.join("harness.sqlite3").is_file());
        assert_eq!(manifest(&root_dir).expect("manifest").apart, [legacy]);
        assert!(!legacy_dir.join(MANIFEST_FILE).exists());
        // The project's memory is global, not either store's: it follows the key.
        assert_eq!(
            project_of(&data, "build-command")["key"],
            json!(context.project.key)
        );
        // `ha doctor` lists the store left apart.
        assert_eq!(
            other_stores(&data, &context.project.key_root, &context.project.key),
            std::slice::from_ref(&legacy_dir)
        );
        assert_eq!(
            super::other_stores_of(&data, &project.repo),
            [legacy_dir.display().to_string()]
        );
        // And every launch from there says so again, changing nothing.
        let again = project.resolve(&project.sub);
        assert_eq!(again.notices, context.notices);
        assert!(legacy_dir.join("harness.sqlite3").is_file());
    }

    #[tokio::test]
    async fn a_store_in_use_is_not_moved_until_it_is_released() {
        let project = Project::new();
        let legacy_dir = project.legacy_dir(&project.sub);
        let writer = store_at(&legacy_dir).await;

        let context = project.resolve(&project.sub);
        assert!(
            legacy_dir.join("harness.sqlite3").is_file(),
            "held: not moved"
        );
        assert!(!context.project_store_dir().join("harness.sqlite3").exists());
        assert!(
            context
                .notices
                .iter()
                .any(|notice| notice.contains("in use")),
            "{:?}",
            context.notices
        );
        writer.close().await.expect("close");

        let context = project.resolve(&project.sub);
        assert!(context.notices.is_empty(), "{:?}", context.notices);
        assert!(
            context
                .project_store_dir()
                .join("harness.sqlite3")
                .is_file()
        );
        assert!(!legacy_dir.exists());
    }

    #[tokio::test]
    async fn launches_racing_to_adopt_one_store_move_it_once() {
        let project = Project::new();
        let legacy_dir = project.legacy_dir(&project.sub);
        make_store(&legacy_dir).await;
        let project = std::sync::Arc::new(project);
        let launches = (0..4)
            .map(|_| {
                let project = std::sync::Arc::clone(&project);
                std::thread::spawn(move || {
                    let sub = project.sub.clone();
                    project.resolve(&sub)
                })
            })
            .collect::<Vec<_>>();
        let contexts = launches
            .into_iter()
            .map(|launch| launch.join().expect("launch"))
            .collect::<Vec<_>>();
        let store_dir = contexts[0].project_store_dir();
        for context in &contexts {
            assert_eq!(context.project_store_dir(), store_dir);
            assert!(context.notices.is_empty(), "{:?}", context.notices);
        }
        assert!(store_dir.join("harness.sqlite3").is_file());
        assert!(!legacy_dir.exists());
        assert_eq!(
            manifest(&store_dir).expect("manifest").adopted.len(),
            1,
            "recorded once"
        );
    }
}
