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
    /// What the repository at `root` is, wherever it is: a store whose root
    /// is gone is found again by it when the repository moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

/// A repository's identity beside its path: its root commits (the commits
/// without a parent), and the URL of its `origin` remote. A clone has the
/// same root commits; a fork has them too, under another remote.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Identity {
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

impl Identity {
    /// Whether two identities are one repository's: the same root commits,
    /// at least one, and the same remote (or none for both).
    fn same_repository(&self, other: &Self) -> bool {
        !self.roots.is_empty() && self.roots == other.roots && self.origin == other.origin
    }
}

/// The git directory whose `config` and refs a checkout at `root` reads: its
/// `.git` directory, or the common directory a `.git` file leads to.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let metadata = std::fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git);
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let target = pointer
        .lines()
        .next()?
        .trim()
        .strip_prefix("gitdir:")?
        .trim();
    let dir = root.join(target);
    match std::fs::read_to_string(dir.join("commondir")) {
        Ok(common) => Some(dir.join(common.trim())),
        Err(_) => Some(dir),
    }
}

/// The URL of the `origin` remote in the repository's config, read from the
/// file (no process): trimmed, without a trailing `/` or `.git`, so two
/// spellings of one remote agree.
fn origin_of(root: &Path) -> Option<String> {
    let config = std::fs::read_to_string(git_dir(root)?.join("config")).ok()?;
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            let section = line.trim_matches(['[', ']']).replace('\'', "\"");
            in_origin = section.split_whitespace().collect::<Vec<_>>() == ["remote", "\"origin\""];
            continue;
        }
        if in_origin
            && let Some((name, value)) = line.split_once('=')
            && name.trim().eq_ignore_ascii_case("url")
        {
            let url = value.trim().trim_matches('"');
            let url = url.trim_end_matches('/');
            let url = url.strip_suffix(".git").unwrap_or(url);
            return (!url.is_empty()).then(|| url.to_owned());
        }
    }
    None
}

/// Whether the repository at `root` has a commit: a branch ref or packed refs.
fn has_commits(root: &Path) -> bool {
    git_dir(root).is_some_and(|dir| {
        dir.join("packed-refs").is_file()
            || std::fs::read_dir(dir.join("refs").join("heads"))
                .is_ok_and(|mut refs| refs.next().is_some())
    })
}

/// The repository's root commits, sorted: one `git rev-list` (the only
/// process this module starts, and only when no store has them recorded).
/// None when git cannot say (no git, no commit).
fn root_commits(root: &Path) -> Vec<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--max-parents=0", "HEAD"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut roots = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.len() >= 40 && line.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

/// The identity of the repository at `root`, from `known` when it holds the
/// root commits already (its remote read again from the config, a file), else
/// computed, which runs git once.
fn identity_of(root: &Path, known: Option<&Identity>) -> Identity {
    let origin = origin_of(root);
    match known {
        Some(known) if !known.roots.is_empty() || !has_commits(root) => Identity {
            roots: known.roots.clone(),
            origin,
        },
        _ => Identity {
            roots: root_commits(root),
            origin,
        },
    }
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
/// beside it rather than deleted; a manifest alone in it is the one this key
/// wrote before it had a store, and goes: the store brings its own.
fn move_store(from: &Path, to: &Path) -> Result<(), String> {
    if to.join(MANIFEST_FILE).is_file() && !has_store(to) {
        let _ = std::fs::remove_file(to.join(MANIFEST_FILE));
    }
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

/// Keep a repository's store when the repository moves. A launch whose key
/// root is a repository records the repository's [`Identity`] in its store's
/// manifest (once: the root commits are kept, the remote read again from the
/// config). When the key has no store yet, a store whose identity is this
/// repository's and whose recorded root no longer exists is the repository's
/// before it moved or was renamed: it is adopted, as a store under an older
/// key is ([`adopt`]). A fork (another remote) never matches, nor a clone that
/// still exists; two such stores are ambiguous and neither moves. Returns the
/// notices the launch shows.
#[must_use]
pub fn follow(data_dir: &Path, root: &Path, key: &str) -> Vec<String> {
    if !root.join(".git").exists() {
        return Vec::new();
    }
    let store_dir = data_dir.join("projects").join(key);
    let kept = manifest(&store_dir);
    let known = kept
        .as_ref()
        .and_then(|manifest| manifest.identity.as_ref());
    let rooted = kept
        .as_ref()
        .is_some_and(|manifest| Path::new(&manifest.root) == root);
    if has_store(&store_dir) {
        // The common case: recorded, and the remote the same (one file read).
        if rooted
            && known.is_some_and(|known| known.origin == origin_of(root) && !known.roots.is_empty())
        {
            return Vec::new();
        }
        let identity = identity_of(root, known);
        if known != Some(&identity) || !rooted {
            remember(&store_dir, root, identity);
        }
        return Vec::new();
    }
    let identity = identity_of(root, known);
    if identity.roots.is_empty() {
        return Vec::new();
    }
    if known != Some(&identity) || !rooted {
        // Kept before the store exists, so the next launch runs no git.
        remember(&store_dir, root, identity.clone());
    }
    let candidates = moved_stores(data_dir, key, &identity);
    match candidates.as_slice() {
        [] => Vec::new(),
        [old] => match take_over(data_dir, root, key, old, "store_followed") {
            Ok(()) => Vec::new(),
            Err(notice) => vec![notice],
        },
        several => {
            harness_types::diag::warn("project", "store_follow_ambiguous")
                .msg("several stores of this repository lost their directory; none was moved")
                .field("stores", several.join(", "))
                .emit();
            vec![format!(
                "{} stores of this repository were kept at directories that are gone ({}); none was moved: `ha project rekey <key>` moves the one to keep",
                several.len(),
                several.join(", ")
            )]
        }
    }
}

/// Record `identity` and `root` in the store directory's manifest.
fn remember(store_dir: &Path, root: &Path, identity: Identity) {
    let mut manifest = manifest(store_dir).unwrap_or_else(|| Manifest {
        schema_version: SCHEMA_VERSION,
        ..Manifest::default()
    });
    manifest.root = root.display().to_string();
    manifest.identity = Some(identity);
    if let Err(error) = std::fs::create_dir_all(store_dir)
        .map_err(|error| error.to_string())
        .and_then(|()| write_manifest(store_dir, &manifest))
    {
        harness_types::diag::warn("project", "manifest_unwritten")
            .msg(error)
            .emit();
    }
}

/// The keys of the stores whose manifest names this repository and whose
/// recorded root is gone.
fn moved_stores(data_dir: &Path, key: &str, identity: &Identity) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(data_dir.join("projects")) else {
        return Vec::new();
    };
    let mut found = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != key)
        .filter(|entry| has_store(&entry.path()))
        .filter_map(|entry| {
            let kept = manifest(&entry.path())?;
            let same = kept
                .identity
                .as_ref()
                .is_some_and(|other| other.same_repository(identity));
            (same && !kept.root.is_empty() && !Path::new(&kept.root).exists())
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    found.sort();
    found
}

/// Move the store kept under `old` to `key`'s place, under the adoption lock,
/// and give its memory and lessons the new key: what [`follow`] and
/// `ha project rekey` do. `event` names the diagnostic of a move. Refused when
/// `key` has a store of its own (two stores that both hold data are never
/// merged) or something holds the old one.
///
/// # Errors
/// The notice that says why it did not move.
pub fn take_over(
    data_dir: &Path,
    root: &Path,
    key: &str,
    old: &str,
    event: &'static str,
) -> Result<(), String> {
    let projects = data_dir.join("projects");
    let store_dir = projects.join(key);
    let old_dir = projects.join(old);
    let _lock =
        lock(data_dir).map_err(|error| format!("the adoption lock could not be taken: {error}"))?;
    if !has_store(&old_dir) {
        return Err(format!("no store is kept at {}", old_dir.display()));
    }
    if has_store(&store_dir) {
        return Err(format!(
            "{} holds a store already, and so does {}; two stores that both hold data are never merged",
            store_dir.display(),
            old_dir.display()
        ));
    }
    if let Some(holder) = held(data_dir, &old_dir) {
        return Err(format!(
            "the store at {} is in use by {holder}; it moves to {} when that ends",
            old_dir.display(),
            store_dir.display()
        ));
    }
    move_store(&old_dir, &store_dir).map_err(|error| {
        harness_types::diag::warn("project", "adoption_failed")
            .msg(error.clone())
            .field("store", store_dir.display().to_string())
            .field("legacy", old_dir.display().to_string())
            .emit();
        format!(
            "the store at {} could not be moved to {}: {error}",
            old_dir.display(),
            store_dir.display()
        )
    })?;
    retag(data_dir, root, key, old, &old_dir, &store_dir);
    record(&store_dir, root, |manifest| &mut manifest.adopted, old);
    harness_types::diag::info("project", event)
        .msg("a store of this project kept under another key moved to its key")
        .field("store", store_dir.display().to_string())
        .field("legacy", old_dir.display().to_string())
        .emit();
    Ok(())
}

/// The key of the store `old` names for `ha project rekey`: a key itself
/// (`project-` and 16 hex digits), or a directory the project was at: its
/// key, the key it had before keys followed the repository, or a store whose
/// manifest names it as its root.
#[must_use]
pub fn store_named(data_dir: &Path, old: &str) -> Option<String> {
    let projects = data_dir.join("projects");
    let is_key = old.strip_prefix("project-").is_some_and(|hex| {
        hex.len() == 16 && hex.chars().all(|character| character.is_ascii_hexdigit())
    });
    if is_key {
        return has_store(&projects.join(old)).then(|| old.to_owned());
    }
    // Spelled with the platform's separators, as a launch spelled it.
    let path = Path::new(old).components().collect::<PathBuf>();
    let keys = [
        super::bootstrap::project_key_of(&path),
        super::bootstrap::legacy_key_of(&path),
    ];
    if let Some(key) = keys.iter().find(|key| has_store(&projects.join(key))) {
        return Some(key.clone());
    }
    let spelled = super::bootstrap::canonical_or_given(&path);
    let fold = |path: &Path| {
        let text = path.components().collect::<PathBuf>().display().to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    let wanted = fold(&spelled);
    // A store names the directory in its manifest, or registered it.
    let mut found = std::fs::read_dir(&projects)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| has_store(&entry.path()))
        .filter(|entry| {
            manifest(&entry.path()).is_some_and(|kept| fold(Path::new(&kept.root)) == wanted)
                || registered_roots(&entry.path())
                    .iter()
                    .any(|root| fold(Path::new(root)) == wanted)
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    found.sort();
    found.into_iter().next()
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

    fn git(directory: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .expect("git runs");
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A repository with one commit and an origin remote.
    fn repository(directory: &Path, origin: &str) {
        std::fs::create_dir_all(directory).expect("dir");
        git(directory, &["init", "-q"]);
        std::fs::write(directory.join("README.md"), "# r\n").expect("readme");
        git(directory, &["add", "-A"]);
        git(directory, &["commit", "-qm", "init"]);
        git(directory, &["remote", "add", "origin", origin]);
    }

    /// A launch from `directory` that leaves a store there, as a conversation
    /// does.
    async fn launch_with_store(project: &Project, directory: &Path) -> LaunchContext {
        let context = project.resolve(directory);
        make_store(&context.project_store_dir()).await;
        // The identity is recorded by the launch, once the store exists too.
        project.resolve(directory)
    }

    #[tokio::test]
    async fn a_moved_repository_finds_its_store_and_a_live_clone_keeps_its_own() {
        let project = Project::new();
        let data = project.data();
        let origin = "https://example.invalid/team/repo.git";
        let first = project.temp.path().join("first");
        let clone = project.temp.path().join("clone");
        repository(&first, origin);
        git(
            project.temp.path(),
            &[
                "clone",
                "-q",
                &first.display().to_string(),
                &clone.display().to_string(),
            ],
        );
        git(
            &clone,
            &["remote", "set-url", "origin", &format!("{origin}/")],
        );
        let before = launch_with_store(&project, &first).await;
        let clone_store = launch_with_store(&project, &clone)
            .await
            .project_store_dir();
        let old_key = before.project.key.clone();
        keep_for(&data, &old_key, &first);
        let identity = manifest(&before.project_store_dir())
            .and_then(|kept| kept.identity)
            .expect("the identity is recorded");
        assert_eq!(identity.roots.len(), 1);
        assert_eq!(
            identity.origin.as_deref(),
            Some("https://example.invalid/team/repo")
        );

        // Moved and renamed.
        let moved = project.temp.path().join("elsewhere").join("renamed");
        std::fs::create_dir_all(moved.parent().expect("parent")).expect("parent");
        std::fs::rename(&first, &moved).expect("move the repository");
        let after = project.resolve(&moved);
        assert!(after.notices.is_empty(), "{:?}", after.notices);
        let store_dir = after.project_store_dir();
        assert_ne!(after.project.key, old_key);
        assert!(
            store_dir.join("harness.sqlite3").is_file(),
            "the store followed"
        );
        assert!(!before.project_store_dir().exists(), "one store, not two");
        assert_eq!(manifest(&store_dir).expect("manifest").adopted, [old_key]);
        assert_eq!(
            project_of(&data, "build-command"),
            json!({"key": after.project.key, "name": "renamed"})
        );
        // The clone that still exists kept its own store.
        assert!(clone_store.join("harness.sqlite3").is_file());
        assert_eq!(project.resolve(&clone).project_store_dir(), clone_store);
        // A second launch changes nothing.
        assert!(project.resolve(&moved).notices.is_empty());
        assert_eq!(project.resolve(&moved).project_store_dir(), store_dir);
    }

    #[tokio::test]
    async fn a_fork_or_a_repository_that_did_not_move_is_never_adopted() {
        let project = Project::new();
        let upstream = project.temp.path().join("upstream");
        repository(&upstream, "https://example.invalid/team/repo.git");
        let store = launch_with_store(&project, &upstream)
            .await
            .project_store_dir();
        // A fork: the same root commit under another remote; the upstream's
        // directory gone.
        let fork = project.temp.path().join("fork");
        git(
            project.temp.path(),
            &[
                "clone",
                "-q",
                &upstream.display().to_string(),
                &fork.display().to_string(),
            ],
        );
        git(
            &fork,
            &[
                "remote",
                "set-url",
                "origin",
                "https://example.invalid/me/repo.git",
            ],
        );
        let aside = project.temp.path().join("upstream-aside");
        std::fs::rename(&upstream, &aside).expect("move");
        let context = project.resolve(&fork);
        assert!(
            store.join("harness.sqlite3").is_file(),
            "the fork took nothing"
        );
        assert!(!context.project_store_dir().join("harness.sqlite3").exists());
        // Back where it was, the repository opens its own store.
        std::fs::rename(&aside, &upstream).expect("back");
        assert_eq!(project.resolve(&upstream).project_store_dir(), store);
    }

    #[tokio::test]
    async fn rekey_moves_a_store_by_hand_and_never_over_one_that_holds_data() {
        let project = Project::new();
        let data = project.data();
        let old = project.temp.path().join("old-place");
        std::fs::create_dir_all(&old).expect("old");
        let old_store = launch_with_store(&project, &old).await.project_store_dir();
        let old_key = old_store
            .file_name()
            .expect("key")
            .to_string_lossy()
            .into_owned();
        let here = project.temp.path().join("here");
        std::fs::create_dir_all(&here).expect("here");
        let key = bootstrap::project_key_of(&here);
        std::fs::remove_dir_all(&old).expect("the old place is gone");
        assert_eq!(
            super::store_named(&data, &old.display().to_string()),
            Some(old_key.clone())
        );
        assert_eq!(super::store_named(&data, &old_key), Some(old_key.clone()));
        // Typed with the other separator, as a shell may.
        assert_eq!(
            super::store_named(&data, &old.display().to_string().replace('\\', "/")),
            Some(old_key.clone())
        );
        super::take_over(
            &data,
            &bootstrap::project_root(&here),
            &key,
            &old_key,
            "store_rekeyed",
        )
        .expect("moved");
        let store_dir = data.join("projects").join(&key);
        assert!(store_dir.join("harness.sqlite3").is_file());
        assert!(!old_store.exists());
        // A store that holds data is never merged into.
        let other = project.temp.path().join("other");
        std::fs::create_dir_all(&other).expect("other");
        let other_store = launch_with_store(&project, &other)
            .await
            .project_store_dir();
        let error = super::take_over(
            &data,
            &bootstrap::project_root(&here),
            &key,
            &other_store.file_name().expect("key").to_string_lossy(),
            "store_rekeyed",
        )
        .expect_err("both hold data");
        assert!(error.contains("never merged"), "{error}");
        assert!(other_store.join("harness.sqlite3").is_file());
    }
}
