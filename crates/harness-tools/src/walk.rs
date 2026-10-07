//! The workspace walk behind `list_files`, `glob`, `search_text` and the
//! workspace fingerprint: parallel, pruned, and cached between calls.
//!
//! Measured on a 2,000-file workspace before this module: one walk per tool
//! call, on one thread, descending into `.git` only to drop every entry it
//! found there, with two metadata calls per file. Now:
//!
//! - the walk runs on `ignore`'s parallel walker (ripgrep's), with a directory
//!   that could only hold protected paths (`.git`, `.harness`, `secrets`...)
//!   pruned before it is read;
//! - the metadata a directory listing already carries is used as it is (on
//!   Windows `FindFirstFileEx` hands it over with the name);
//! - a finished walk is kept, and used again until something in the
//!   workspace changes: this process's own writes say so at once, a file
//!   watcher says so for every other writer, and without a watcher a walk is
//!   trusted for one second only.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use harness_types::{ErrorCode, HarnessError};
use ignore::{DirEntry, ParallelVisitor, ParallelVisitorBuilder, WalkBuilder, WalkState};

/// One file a walk found: where it is, and the size and time that tell
/// whether its cached hash or index entry is still its own.
#[derive(Clone, Debug)]
pub(crate) struct WalkFile {
    pub absolute: PathBuf,
    /// Relative to the walked base, with `/` separators.
    pub relative: String,
    pub len: u64,
    pub modified: Option<SystemTime>,
}

/// What a walk found, and whether it found everything.
#[derive(Clone, Debug)]
pub(crate) struct Walk {
    pub files: Vec<WalkFile>,
    /// `false` when the walk stopped at its entry bound or its deadline.
    pub complete: bool,
    /// Every directory the walk read: a change anywhere else (an ignored
    /// build folder, `.git`) cannot change what the walk returns.
    pub directories: HashSet<PathBuf>,
}

/// Most files one walk returns before it stops and says it is incomplete.
pub(crate) const MAX_WALK_FILES: usize = 200_000;

/// How long a walk for a tool may take before it stops and returns what it
/// has: a walk started in a home folder took 32 seconds to finish.
pub(crate) const TOOL_WALK_DEADLINE: Duration = Duration::from_secs(10);

/// How long a cached walk is trusted when no watcher reports changes.
const UNWATCHED_TTL: Duration = Duration::from_secs(1);

/// A path component no tool may list, read or search: the guard of
/// [`crate::workspace::is_sensitive_workspace_path`] for one name. A
/// directory with such a name only holds protected paths, so the walk never
/// opens it.
pub(crate) fn sensitive_component(name: &str) -> bool {
    let value = name.to_ascii_lowercase();
    matches!(value.as_str(), ".git" | ".harness" | ".env")
        || value.starts_with(".env.")
        || value.contains("credential")
        || value.contains("secret")
        || value.contains("password")
        || value.contains("private_key")
}

/// Walk `base` the way every workspace tool sees it: `.gitignore` honored in
/// and outside a repository, hidden files included, links never followed,
/// protected paths left out, in path order.
pub(crate) fn walk_files_within(
    base: &Path,
    bound: usize,
    deadline: Option<Instant>,
) -> Result<Walk, HarnessError> {
    let threads = std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .clamp(2, 12);
    let walker = WalkBuilder::new(base)
        .hidden(false)
        .follow_links(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .threads(threads)
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_some_and(|kind| kind.is_dir())
                || !sensitive_component(&entry.file_name().to_string_lossy())
        })
        .build_parallel();
    let shared = Shared {
        base: base.to_owned(),
        bound,
        deadline,
        count: AtomicUsize::new(0),
        stopped: AtomicBool::new(false),
        failure: Mutex::new(None),
        results: Mutex::new(Vec::new()),
    };
    let mut builder = CollectorBuilder { shared: &shared };
    walker.visit(&mut builder);
    if let Some(error) = shared
        .failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    {
        return Err(error);
    }
    let mut files = Vec::new();
    let mut directories = HashSet::new();
    directories.insert(base.to_owned());
    for (mut local_files, local_directories) in shared
        .results
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        files.append(&mut local_files);
        directories.extend(local_directories);
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    if files.len() > bound {
        files.truncate(bound);
    }
    Ok(Walk {
        files,
        complete: !shared.stopped.load(Ordering::SeqCst),
        directories,
    })
}

struct Shared {
    base: PathBuf,
    bound: usize,
    deadline: Option<Instant>,
    count: AtomicUsize,
    stopped: AtomicBool,
    failure: Mutex<Option<HarnessError>>,
    results: Mutex<Vec<(Vec<WalkFile>, Vec<PathBuf>)>>,
}

struct CollectorBuilder<'s> {
    shared: &'s Shared,
}

impl<'s> ParallelVisitorBuilder<'s> for CollectorBuilder<'s> {
    fn build(&mut self) -> Box<dyn ParallelVisitor + 's> {
        Box::new(Collector {
            shared: self.shared,
            files: Vec::new(),
            directories: Vec::new(),
        })
    }
}

/// One walker thread: what it found stays local until the thread is done.
struct Collector<'s> {
    shared: &'s Shared,
    files: Vec<WalkFile>,
    directories: Vec<PathBuf>,
}

impl Drop for Collector<'_> {
    fn drop(&mut self) {
        let files = std::mem::take(&mut self.files);
        let directories = std::mem::take(&mut self.directories);
        self.shared
            .results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((files, directories));
    }
}

impl ParallelVisitor for Collector<'_> {
    fn visit(&mut self, entry: Result<DirEntry, ignore::Error>) -> WalkState {
        match self.entry(entry) {
            Ok(state) => state,
            Err(error) => {
                let mut failure = self
                    .shared
                    .failure
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                failure.get_or_insert(error);
                WalkState::Quit
            }
        }
    }
}

impl Collector<'_> {
    fn entry(&mut self, entry: Result<DirEntry, ignore::Error>) -> Result<WalkState, HarnessError> {
        let shared = self.shared;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // The walker wraps the failing path in its own error types, so
                // recover the location instead of leaving the message anonymous.
                let path = match &error {
                    ignore::Error::WithPath { path, .. } => path.clone(),
                    _ => shared.base.clone(),
                };
                return Err(walk_failure(&path, &error));
            }
        };
        if entry.depth() == 0 {
            return Ok(WalkState::Continue);
        }
        if shared
            .deadline
            .is_some_and(|deadline| Instant::now() > deadline)
        {
            shared.stopped.store(true, Ordering::SeqCst);
            return Ok(WalkState::Quit);
        }
        let path = entry.path();
        let Some(kind) = entry.file_type() else {
            return Ok(WalkState::Continue);
        };
        if kind.is_dir() {
            self.directories.push(path.to_owned());
            return Ok(WalkState::Continue);
        }
        if kind.is_symlink() {
            return Ok(WalkState::Continue);
        }
        // `DirEntry::metadata` is the listing's own on Windows, and an `lstat`
        // elsewhere: links are never followed.
        let metadata = entry.metadata().map_err(|error| match error.io_error() {
            Some(io) => crate::workspace::entry_failure(path, io),
            None => walk_failure(path, &error),
        })?;
        if is_reparse(&metadata) || !metadata.is_file() {
            return Ok(WalkState::Continue);
        }
        let Ok(relative) = path.strip_prefix(&shared.base) else {
            return Err(HarnessError::new(
                ErrorCode::WorkspaceEscape,
                "workspace walk escaped root",
            ));
        };
        if crate::workspace::is_sensitive_workspace_path(relative) {
            return Ok(WalkState::Continue);
        }
        if shared.count.fetch_add(1, Ordering::SeqCst) >= shared.bound {
            shared.stopped.store(true, Ordering::SeqCst);
            return Ok(WalkState::Quit);
        }
        self.files.push(WalkFile {
            absolute: path.to_owned(),
            relative: relative_text(relative),
            len: metadata.len(),
            modified: metadata.modified().ok(),
        });
        Ok(WalkState::Continue)
    }
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub(crate) fn walk_failure(path: &Path, error: &ignore::Error) -> HarnessError {
    if error
        .io_error()
        .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::PermissionDenied)
    {
        return crate::workspace::deny_read_error(path, error);
    }
    HarnessError::new(
        ErrorCode::WorkspaceEscape,
        format!("workspace walk failed: {error}"),
    )
}

pub(crate) fn relative_text(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

// ---------------------------------------------------------------------------
// Change tracking: the generation, and the watcher that moves it
// ---------------------------------------------------------------------------

/// Moves on every change that can alter a walk, a file hash or an index
/// entry. A cache stamped with an older generation is not used.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// The current workspace generation.
pub(crate) fn generation() -> u64 {
    GENERATION.load(Ordering::SeqCst)
}

/// Something in a workspace may have changed: this process wrote a file, a
/// process it ran has finished, or a watcher saw a change.
pub(crate) fn note_change() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

struct Watched {
    /// Kept alive for as long as the process runs; dropping it stops events.
    _watcher: Option<notify::RecommendedWatcher>,
    /// Whether events arrive. A root whose watcher failed falls back to the
    /// one-second trust of an unwatched walk.
    active: bool,
}

fn watched_roots() -> &'static Mutex<HashMap<PathBuf, Watched>> {
    static ROOTS: OnceLock<Mutex<HashMap<PathBuf, Watched>>> = OnceLock::new();
    ROOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The directories, per watched root, a change in which can matter: the ones
/// the last walks read. A build writing thousands of files into an ignored
/// `target` folder moves nothing.
fn relevant_directories() -> &'static Mutex<HashMap<PathBuf, HashSet<PathBuf>>> {
    static DIRECTORIES: OnceLock<Mutex<HashMap<PathBuf, HashSet<PathBuf>>>> = OnceLock::new();
    DIRECTORIES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a change at `path` can alter what a walk of `root` returns.
fn relevant(root: &Path, path: &Path, directories: &HashSet<PathBuf>) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    if relative
        .components()
        .next()
        .is_some_and(|first| first.as_os_str().eq_ignore_ascii_case(".git"))
    {
        return false;
    }
    directories.contains(path)
        || path
            .parent()
            .is_some_and(|parent| directories.contains(parent))
}

/// Start watching `root` once per process. Returns whether changes under it
/// are reported.
pub(crate) fn ensure_watched(root: &Path) -> bool {
    use notify::Watcher as _;

    if std::env::var_os("HA_FILE_WATCH").is_some_and(|value| value == "off") {
        return false;
    }
    let mut roots = watched_roots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(watched) = roots.get(root) {
        return watched.active;
    }
    let owned_root = root.to_owned();
    let handler = move |event: notify::Result<notify::Event>| match event {
        Ok(event) => {
            // Events for one watch arrive in the order they happened, and this
            // handler takes them one at a time: when a sentinel shows up here,
            // every change made before it was written has been handled.
            mark_sentinels(&event.paths);
            if event.need_rescan() {
                note_change();
                return;
            }
            let directories = relevant_directories()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let empty = HashSet::new();
            let known = directories.get(&owned_root).unwrap_or(&empty);
            if event
                .paths
                .iter()
                .any(|path| relevant(&owned_root, path, known))
            {
                note_change();
            }
        }
        // A watcher that lost events cannot vouch for any cache.
        Err(_) => note_change(),
    };
    let started = notify::recommended_watcher(handler).and_then(|mut watcher| {
        watcher.watch(root, notify::RecursiveMode::Recursive)?;
        Ok(watcher)
    });
    let watched = match started {
        Ok(watcher) => Watched {
            _watcher: Some(watcher),
            active: true,
        },
        Err(_) => Watched {
            _watcher: None,
            active: false,
        },
    };
    let active = watched.active;
    roots.insert(root.to_owned(), watched);
    active
}

/// How long [`settle_changes`] waits for the watcher to report its sentinel
/// before it gives up and invalidates every cache instead.
const SENTINEL_WAIT: Duration = Duration::from_millis(200);

/// Sentinels written and not yet taken back, by file name: whether the
/// watcher has reported each. A name not in here (a sentinel's own deletion,
/// say) is nobody's business.
fn sentinels() -> &'static (Mutex<HashMap<OsString, bool>>, Condvar) {
    static SENTINELS: OnceLock<(Mutex<HashMap<OsString, bool>>, Condvar)> = OnceLock::new();
    SENTINELS.get_or_init(|| (Mutex::new(HashMap::new()), Condvar::new()))
}

fn mark_sentinels(paths: &[PathBuf]) {
    let (pending, arrived) = sentinels();
    let mut pending = pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_empty() {
        return;
    }
    let mut marked = false;
    for name in paths.iter().filter_map(|path| path.file_name()) {
        if let Some(seen) = pending.get_mut(name) {
            *seen = true;
            marked = true;
        }
    }
    if marked {
        arrived.notify_all();
    }
}

/// A process this host ran (a shell command, a hook, an extension tool) has
/// finished, and may have changed files under `root`.
///
/// Noting a change outright made the next walk - the after-fingerprint of
/// the very call that ran the process - walk the whole workspace again, even
/// after `cargo test` or `git status` that changed nothing a walk returns.
/// When the root's watcher is running, the watcher already reports every
/// change that matters; what is missing is knowing it has caught up. So a
/// sentinel file is written where the watcher sees it but no walk looks
/// (`.harness` or `.git`, both pruned), and this waits for the watcher to
/// report it: events for one watch are delivered in order, so by then every
/// change the process made before it exited has moved the generation if it
/// could matter. Anything that keeps that from being sure - no watcher, no
/// such folder, a sentinel not seen in time - notes a change, as before.
pub(crate) async fn settle_changes(root: &Path) {
    let root = root.to_owned();
    let synced = tokio::task::spawn_blocking(move || sync_with_watcher(&root, SENTINEL_WAIT))
        .await
        .unwrap_or(false);
    if !synced {
        note_change();
    }
}

/// Whether the watcher of `root` has reported everything that happened under
/// it before this call; see [`settle_changes`].
fn sync_with_watcher(root: &Path, wait: Duration) -> bool {
    static NEXT: AtomicU64 = AtomicU64::new(0);

    if !watcher_active(root) {
        return false;
    }
    // Only a real folder: a junction's contents are not reported by a watch
    // on the folder that holds it, so a sentinel there would never arrive.
    let Some(folder) = [".harness", ".git"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|folder| fs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir()))
    else {
        return false;
    };
    let name = OsString::from(format!(
        "ha-sync-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let path = folder.join(&name);
    let (pending, arrived) = sentinels();
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(name.clone(), false);
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .is_ok();
    let mut seen = false;
    if created {
        let guard = pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (guard, _) = arrived
            .wait_timeout_while(guard, wait, |pending| {
                !pending.get(&name).copied().unwrap_or(false)
            })
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        seen = guard.get(&name).copied().unwrap_or(false);
    }
    pending
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&name);
    if created {
        let _ = fs::remove_file(&path);
    }
    seen
}

/// Whether `root` (as given, or canonical) has a watcher that reports events.
/// It is never started here: a watcher started after the process ran would
/// have missed what it did.
fn watcher_active(root: &Path) -> bool {
    let roots = watched_roots()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(watched) = roots.get(root) {
        return watched.active;
    }
    fs::canonicalize(root)
        .ok()
        .and_then(|canonical| roots.get(&canonical).map(|watched| watched.active))
        .unwrap_or(false)
}

fn remember_directories(root: &Path, base: &Path, walk: &Walk) {
    let reset = {
        let mut directories = relevant_directories()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let known = directories.entry(root.to_owned()).or_default();
        // Directories deleted or renamed since earlier walks were never
        // removed, so a long session kept growing the set. Once it holds far
        // more than a complete walk of the whole root just saw, that walk
        // replaces it.
        let reset = base == root
            && walk.complete
            && known.len() > walk.directories.len().saturating_mul(2) + 1024;
        if reset {
            known.clear();
        }
        known.extend(walk.directories.iter().cloned());
        reset
    };
    if reset {
        // A cached walk of a narrower base may have registered directories
        // the root walk does not visit (an ignored folder walked on its own);
        // with them gone, its changes would no longer invalidate it, so it is
        // dropped and walked again when asked for.
        walk_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|cached_base, _| cached_base == root || !cached_base.starts_with(root));
    }
}

// ---------------------------------------------------------------------------
// The walk cache
// ---------------------------------------------------------------------------

struct CachedWalk {
    walk: Arc<Walk>,
    generation: u64,
    built: Instant,
    watched: bool,
}

impl CachedWalk {
    fn fresh(&self) -> bool {
        self.generation == generation() && (self.watched || self.built.elapsed() < UNWATCHED_TTL)
    }
}

fn walk_cache() -> &'static Mutex<HashMap<PathBuf, CachedWalk>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedWalk>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The walk of `base` inside the workspace `root`: from the cache when nothing
/// changed since it was taken, from a fresh walk of `root` narrowed to `base`
/// when one is cached, and walked otherwise. Only a complete walk is cached.
pub(crate) fn cached_walk(
    root: &Path,
    base: &Path,
    deadline: Option<Instant>,
) -> Result<Arc<Walk>, HarnessError> {
    let watched = ensure_watched(root);
    {
        let cache = walk_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cached) = cache.get(base).filter(|cached| cached.fresh()) {
            return Ok(Arc::clone(&cached.walk));
        }
        if base != root
            && let Some(cached) = cache.get(root).filter(|cached| cached.fresh())
        {
            return Ok(Arc::new(narrow(&cached.walk, base)));
        }
    }
    // The generation is read before the walk: a change during it leaves the
    // result stamped old, so it is not trusted for longer than it saw.
    let stamp = generation();
    let walk = walk_files_within(base, MAX_WALK_FILES, deadline)?;
    remember_directories(root, base, &walk);
    let walk = Arc::new(walk);
    if walk.complete {
        let mut cache = walk_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() > 32 {
            cache.retain(|_, cached| cached.fresh());
        }
        cache.insert(
            base.to_owned(),
            CachedWalk {
                walk: Arc::clone(&walk),
                generation: stamp,
                built: Instant::now(),
                watched,
            },
        );
    }
    Ok(walk)
}

/// The part of a root walk below `base`, relative to `base`.
fn narrow(walk: &Walk, base: &Path) -> Walk {
    let files = walk
        .files
        .iter()
        .filter_map(|file| {
            let relative = file.absolute.strip_prefix(base).ok()?;
            Some(WalkFile {
                absolute: file.absolute.clone(),
                relative: relative_text(relative),
                len: file.len,
                modified: file.modified,
            })
        })
        .collect::<Vec<_>>();
    let mut files = files;
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Walk {
        files,
        complete: walk.complete,
        directories: walk
            .directories
            .iter()
            .filter(|directory| directory.starts_with(base))
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("walk-{}", harness_types::InputId::generate()));
        fs::create_dir_all(root.join("src").join("deep")).expect("root");
        fs::create_dir_all(root.join("target").join("debug")).expect("target");
        fs::create_dir_all(root.join("secrets")).expect("secrets");
        fs::create_dir_all(root.join(".git")).expect("git");
        fs::write(root.join(".gitignore"), "target/\n").expect("ignore");
        fs::write(root.join("src").join("main.rs"), "fn main() {}\n").expect("main");
        fs::write(
            root.join("src").join("deep").join("lib.rs"),
            "pub fn a() {}\n",
        )
        .expect("lib");
        fs::write(root.join("target").join("debug").join("out.o"), "x").expect("out");
        fs::write(root.join("secrets").join("token.txt"), "x").expect("secret");
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("head");
        fs::canonicalize(root).expect("canonical")
    }

    #[test]
    fn the_walk_is_sorted_ignores_build_output_and_never_opens_protected_folders() {
        let root = workspace();
        let walk = walk_files_within(&root, 100, None).expect("walk");
        let names = walk
            .files
            .iter()
            .map(|file| file.relative.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, [".gitignore", "src/deep/lib.rs", "src/main.rs"]);
        assert!(walk.complete);
        assert!(!walk.directories.contains(&root.join(".git")));
        assert!(!walk.directories.contains(&root.join("secrets")));
        assert!(walk.directories.contains(&root.join("src").join("deep")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_walk_past_its_bound_is_incomplete_not_an_error() {
        let root = workspace();
        let walk = walk_files_within(&root, 2, None).expect("walk");
        assert!(!walk.complete);
        assert_eq!(walk.files.len(), 2);
        let late = walk_files_within(
            &root,
            100,
            Instant::now().checked_sub(Duration::from_secs(1)),
        )
        .expect("walk");
        assert!(!late.complete);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_cached_walk_is_used_until_a_change_is_noted() {
        let root = workspace();
        let first = cached_walk(&root, &root, None).expect("walk");
        let again = cached_walk(&root, &root, None).expect("walk");
        if again.files.len() == first.files.len() && first.complete {
            // The same walk, unless a second passed without a watcher.
            assert!(Arc::ptr_eq(&first, &again) || !ensure_watched(&root));
        }
        fs::write(root.join("src").join("new.rs"), "x").expect("new");
        note_change();
        let after = cached_walk(&root, &root, None).expect("walk");
        assert!(after.files.iter().any(|file| file.relative == "src/new.rs"));
        let below = cached_walk(&root, &root.join("src"), None).expect("narrowed");
        let names = below
            .files
            .iter()
            .map(|file| file.relative.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["deep/lib.rs", "main.rs", "new.rs"]);
        let _ = fs::remove_dir_all(root);
    }

    /// Run `command` in `root` through the platform shell, as a tool call's
    /// process would: a writer that never notes a change itself.
    fn run_in(root: &Path, command: &str) {
        let status = if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", command])
                .current_dir(root)
                .status()
        } else {
            std::process::Command::new("sh")
                .args(["-c", command])
                .current_dir(root)
                .status()
        };
        assert!(status.expect("shell").success(), "{command}");
    }

    #[tokio::test]
    async fn a_file_a_process_wrote_is_in_the_next_walk_after_it_settles() {
        let root = workspace();
        let first = cached_walk(&root, &root, None).expect("walk");
        assert!(
            !first
                .files
                .iter()
                .any(|file| file.relative == "src/made.rs")
        );
        run_in(&root.join("src"), "echo made> made.rs");
        settle_changes(&root).await;
        let after = cached_walk(&root, &root, None).expect("walk");
        assert!(
            after
                .files
                .iter()
                .any(|file| file.relative == "src/made.rs"),
            "the process's file is walked"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_process_that_only_reads_leaves_the_cached_walk_trusted() {
        let root = workspace();
        let _ = cached_walk(&root, &root, None).expect("walk");
        if !ensure_watched(&root) {
            // No watcher (HA_FILE_WATCH=off, or none on this file system):
            // settling notes a change, which the other test covers.
            let _ = fs::remove_dir_all(root);
            return;
        }
        let reader = if cfg!(windows) {
            "type src\\main.rs"
        } else {
            "cat src/main.rs"
        };
        // Other tests note changes at any moment (the generation is
        // process-wide), so the reuse is checked on a quiet attempt.
        let reused = (0..20).any(|_| {
            let before = cached_walk(&root, &root, None).expect("walk");
            let stamp = generation();
            run_in(&root, reader);
            // The wait is generous only so a loaded machine does not fail it:
            // the sentinel arriving is under test, not its latency.
            assert!(
                sync_with_watcher(&root, Duration::from_secs(10)),
                "the watcher reported the sentinel, so no change is noted"
            );
            let again = cached_walk(&root, &root, None).expect("walk");
            generation() == stamp && Arc::ptr_eq(&before, &again)
        });
        assert!(reused, "a settled read-only process keeps the walk cached");
        assert!(
            fs::read_dir(root.join(".git"))
                .expect("git")
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with("ha-sync-")),
            "the sentinel is taken back"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_changes_in_walked_folders_are_relevant() {
        let root = PathBuf::from("/w");
        let directories = HashSet::from([root.clone(), root.join("src")]);
        assert!(relevant(
            &root,
            &root.join("src").join("a.rs"),
            &directories
        ));
        assert!(relevant(&root, &root.join("new_dir"), &directories));
        assert!(!relevant(
            &root,
            &root.join("target").join("x.o"),
            &directories
        ));
        assert!(!relevant(
            &root,
            &root.join(".git").join("index"),
            &directories
        ));
    }
}
