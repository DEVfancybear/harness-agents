//! Host-owned isolated worker workspaces.
//!
//! The host owns every shared Git metadata operation. A worker receives one
//! worktree on one branch and never writes to another worker's branch or to the
//! user's checkout. Worktrees isolate concurrent edits; they are explicitly not
//! a security boundary.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex as StdMutex, OnceLock},
    time::{Duration, SystemTime},
};

use harness_types::{AgentRunId, ContentHash, ErrorCode, ProjectId, TaskId};
use tokio::sync::Mutex;

use crate::contracts::{
    DirtyReason, OrchestratorError, VerifiedSnapshot, WorktreeRecord, WorktreeState,
};

/// Why a proposed worker change was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopeViolation {
    pub path: String,
    pub reason: String,
}

/// The outcome of inspecting a candidate repository input.
#[derive(Clone, Debug)]
pub enum InputInspection {
    Clean(VerifiedSnapshot),
    Dirty(Vec<DirtyReason>),
}

/// Result of comparing a worker's branch with the integration base.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChangeSet {
    pub paths: Vec<String>,
    pub insertions: u64,
    pub deletions: u64,
}

impl ChangeSet {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

/// Host-owned workspace manager for delegated editing work.
pub struct WorkspaceManager {
    /// Serializes every operation that touches shared Git metadata.
    git_lock: Arc<Mutex<()>>,
    state_root: PathBuf,
    /// Verified repository registrations. A project identity is resolved from a
    /// registration, never from the caller's current working directory.
    registrations: StdMutex<BTreeMap<String, ProjectId>>,
}

impl WorkspaceManager {
    #[must_use]
    pub fn new(state_root: impl Into<PathBuf>) -> Self {
        Self {
            git_lock: Arc::new(Mutex::new(())),
            state_root: state_root.into(),
            registrations: StdMutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// The lock that serializes shared Git metadata operations.
    ///
    /// Every component that runs Git against the host-owned clone must take
    /// this same lock; a second lock would let an integration fetch/merge race a
    /// worktree creation on the same repository.
    #[must_use]
    pub fn git_lock(&self) -> Arc<Mutex<()>> {
        Arc::clone(&self.git_lock)
    }

    /// Resolve the project identity for a verified repository root.
    ///
    /// The first verified inspection of a canonical root records a
    /// registration; later work in the same root - including every linked
    /// worktree created from it - resolves to that same identity. A different
    /// root is a different project even when its content matches.
    pub fn register_project(&self, root: impl AsRef<Path>, project_id: ProjectId) -> ProjectId {
        let key = canonical_key(root.as_ref());
        let mut registrations = self
            .registrations
            .lock()
            .expect("workspace registration mutex is not poisoned");
        registrations.entry(key).or_insert(project_id).clone()
    }

    /// The project identity already recorded for a root, if any.
    #[must_use]
    pub fn registered_project(&self, root: impl AsRef<Path>) -> Option<ProjectId> {
        let key = canonical_key(root.as_ref());
        self.registrations
            .lock()
            .ok()
            .and_then(|registrations| registrations.get(&key).cloned())
    }

    /// Inspect a repository input. Only a clean tree with an attached branch is
    /// accepted for editing delegation; a dirty tree is refused with reasons and
    /// never stashed, reset or discarded.
    pub async fn inspect_input(
        &self,
        root: impl AsRef<Path>,
        project_id: &ProjectId,
    ) -> Result<InputInspection, OrchestratorError> {
        let root = root.as_ref().to_path_buf();
        let _guard = self.git_lock.lock().await;
        let inspected = {
            let root = root.clone();
            blocking(move || inspect_locked(&root)).await?
        };
        let (head, branch, fingerprint) = match inspected {
            Inspected::Dirty(reasons) => return Ok(InputInspection::Dirty(reasons)),
            Inspected::Clean {
                head,
                branch,
                fingerprint,
            } => (head, branch, fingerprint),
        };
        // A verified inspection is what creates the registration, so a linked
        // worktree created from this snapshot keeps the same project identity.
        let project_id = self.register_project(&root, project_id.clone());
        Ok(InputInspection::Clean(VerifiedSnapshot {
            project_id,
            root: root.to_string_lossy().into_owned(),
            base_commit: head,
            base_branch: branch,
            fingerprint,
        }))
    }

    /// Deterministic fingerprint of a repository root: HEAD plus every tracked
    /// or untracked-but-not-ignored file content hash.
    pub async fn fingerprint(
        &self,
        root: impl AsRef<Path>,
    ) -> Result<ContentHash, OrchestratorError> {
        let root = root.as_ref().to_path_buf();
        let _guard = self.git_lock.lock().await;
        blocking(move || fingerprint_locked(&root)).await
    }

    /// Create one editing worktree for one worker on its own branch.
    pub async fn create_worktree(
        &self,
        snapshot: &VerifiedSnapshot,
        task_id: &TaskId,
        run_id: &AgentRunId,
        write_scope: &[String],
        generation: u64,
    ) -> Result<WorktreeRecord, OrchestratorError> {
        if write_scope.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                "an editing worktree requires a non-empty write scope",
            ));
        }
        let _guard = self.git_lock.lock().await;
        let source = PathBuf::from(&snapshot.root);
        let suffix = task_id
            .as_str()
            .rsplit('-')
            .next()
            .unwrap_or("task")
            .to_owned();
        let worktree_id = format!("wt-{suffix}");
        let path = self.state_root.join("worktrees").join(&worktree_id);
        let branch = format!("harness/p5/{suffix}");
        let base_branch = format!("harness/base/{suffix}");
        {
            let (source, path) = (source.clone(), path.clone());
            let (branch, base_branch) = (branch.clone(), base_branch.clone());
            let clone_source = self.state_root.join("integration").join("source.git");
            let base_commit = snapshot.base_commit.clone();
            blocking(move || {
                add_worktree_locked(
                    &source,
                    &clone_source,
                    &path,
                    &branch,
                    &base_branch,
                    &base_commit,
                )
            })
            .await?;
        }
        // The verified snapshot is the only source of the project identity.
        let project_id = self
            .registered_project(&source)
            .unwrap_or_else(|| snapshot.project_id.clone());
        let record = WorktreeRecord {
            worktree_id,
            task_id: task_id.clone(),
            run_id: run_id.clone(),
            project_id,
            base_commit: snapshot.base_commit.clone(),
            base_branch,
            branch,
            path: path.to_string_lossy().into_owned(),
            write_scope: write_scope.to_vec(),
            state: WorktreeState::Ready,
            input_fingerprint: snapshot.fingerprint.clone(),
            result_fingerprint: None,
            generation,
        };
        Ok(record)
    }

    /// Reject any change outside the worker's declared write scope.
    pub async fn assert_write_scope(
        &self,
        worktree: &str,
        write_scope: &[String],
    ) -> Result<ChangeSet, OrchestratorError> {
        let root = PathBuf::from(worktree);
        let _guard = self.git_lock.lock().await;
        let changes = pending_changes(&root)?;
        let violations = scope_violations(&changes.paths, write_scope);
        if let Some(violation) = violations.first() {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                format!(
                    "worker changed {} outside its write scope: {}",
                    violation.path, violation.reason
                ),
            ));
        }
        Ok(changes)
    }

    /// Commit a worker's scoped changes and record the resulting revision.
    pub async fn commit_worker_changes(
        &self,
        worktree: &WorktreeRecord,
        message: &str,
    ) -> Result<String, OrchestratorError> {
        let root = PathBuf::from(&worktree.path);
        let _guard = self.git_lock.lock().await;
        let changes = pending_changes(&root)?;
        if changes.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a worker that produced no change has no revision to integrate",
            ));
        }
        // Stage first and validate exactly what will be committed. Checking the
        // unstaged snapshot and then running `git add --all` would let a file
        // created after the check enter the commit without a scope decision.
        git(&root, &["add", "--all"])?;
        let staged = staged_changes(&root)?;
        if staged.is_empty() {
            return Err(OrchestratorError::new(
                ErrorCode::ResultIncomplete,
                "a worker that produced no change has no revision to integrate",
            ));
        }
        let violations = scope_violations(&staged.paths, &worktree.write_scope);
        if let Some(violation) = violations.first() {
            return Err(OrchestratorError::new(
                ErrorCode::ScopeAuthorityDenied,
                format!(
                    "worker changed {} outside its write scope: {}",
                    violation.path, violation.reason
                ),
            ));
        }
        git(&root, &["commit", "--quiet", "-m", message])?;
        let revision = git(&root, &["rev-parse", "HEAD"])?.trim().to_owned();
        Ok(revision)
    }

    /// Remove a worktree and its branch. Never touches the user's checkout.
    pub async fn remove_worktree(
        &self,
        record: &WorktreeRecord,
        source_root: &str,
    ) -> Result<(), OrchestratorError> {
        let _guard = self.git_lock.lock().await;
        let source = PathBuf::from(source_root);
        let clone_source = self.state_root.join("integration").join("source.git");
        if clone_source.is_dir() {
            let _ = git(
                &clone_source,
                &["worktree", "remove", "--force", &record.path],
            );
        }
        let _ = source;
        Ok(())
    }

    /// Compute the change set of a branch relative to a base commit.
    pub async fn branch_changes(
        &self,
        worktree: &str,
        base_commit: &str,
    ) -> Result<ChangeSet, OrchestratorError> {
        let root = PathBuf::from(worktree);
        let _guard = self.git_lock.lock().await;
        let range = format!("{base_commit}..HEAD");
        let names = git(&root, &["diff", "--name-only", &range])?;
        let paths = names
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let stats = git(&root, &["diff", "--shortstat", &range])?;
        let (insertions, deletions) = parse_shortstat(&stats);
        Ok(ChangeSet {
            paths,
            insertions,
            deletions,
        })
    }
}

/// Run Git work off the async workers: every call blocks on a child process,
/// and a fingerprint reads the whole tree.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, OrchestratorError> + Send + 'static,
) -> Result<T, OrchestratorError> {
    tokio::task::spawn_blocking(work).await.map_err(|error| {
        OrchestratorError::new(
            ErrorCode::ProcessCanceled,
            format!("workspace inspection did not finish: {error}"),
        )
    })?
}

enum Inspected {
    Dirty(Vec<DirtyReason>),
    Clean {
        head: String,
        branch: String,
        fingerprint: ContentHash,
    },
}

/// The blocking body of [`WorkspaceManager::inspect_input`], under the Git lock.
fn inspect_locked(root: &Path) -> Result<Inspected, OrchestratorError> {
    if !git_ok(root, &["rev-parse", "--git-dir"]) {
        return Ok(Inspected::Dirty(vec![DirtyReason::NotARepository]));
    }
    // Status, the HEAD names and the file listing are independent reads, so
    // they run side by side; the names and listing go unused for a dirty tree.
    let (status, names, listing) = std::thread::scope(|scope| {
        let names = scope.spawn(|| git(root, &["rev-parse", "HEAD", "--abbrev-ref", "HEAD"]));
        let listing = scope.spawn(|| list_files(root));
        let status = git(root, &["status", "--porcelain=v1", "--untracked-files=all"]);
        (status, join_git(names), join_git(listing))
    });
    let status = status?;
    let mut reasons = Vec::new();
    for line in status.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let code = line.get(0..2).unwrap_or("??");
        let path = line.get(3..).unwrap_or("").trim().to_owned();
        let reason = if code.starts_with("??") {
            DirtyReason::UntrackedFile { path }
        } else if code.chars().next().is_some_and(|c| c != ' ' && c != '?') {
            DirtyReason::StagedChange { path }
        } else {
            DirtyReason::TrackedModification { path }
        };
        reasons.push(reason);
    }
    if !reasons.is_empty() {
        return Ok(Inspected::Dirty(reasons));
    }
    let names = names?;
    let mut names = names.lines().map(str::trim);
    let head = names.next().unwrap_or_default().to_owned();
    let branch = names.next().unwrap_or_default().to_owned();
    if branch == "HEAD" || branch.is_empty() {
        return Ok(Inspected::Dirty(vec![DirtyReason::DetachedHead]));
    }
    let fingerprint = fingerprint_of(root, &head, &listing?)?;
    Ok(Inspected::Clean {
        head,
        branch,
        fingerprint,
    })
}

fn join_git<T>(
    handle: std::thread::ScopedJoinHandle<'_, Result<T, OrchestratorError>>,
) -> Result<T, OrchestratorError> {
    handle.join().unwrap_or_else(|_| {
        Err(OrchestratorError::new(
            ErrorCode::ProcessCanceled,
            "a git query panicked",
        ))
    })
}

/// The identity worker commits are made with.
const WORKER_EMAIL: &str = "harness-p5@localhost";
const WORKER_NAME: &str = "harness-p5";

/// The blocking body of [`WorkspaceManager::create_worktree`], under the Git lock.
fn add_worktree_locked(
    source: &Path,
    clone_source: &Path,
    path: &Path,
    branch: &str,
    base_branch: &str,
    base_commit: &str,
) -> Result<(), OrchestratorError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot create worktree parent: {error}"),
            )
        })?;
    }
    if let Some(parent) = clone_source.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot create integration root: {error}"),
            )
        })?;
    }
    // Clone from the verified snapshot rather than reusing the user's
    // checkout, so the worker has an isolated object store.
    if !clone_source.is_dir() {
        git(
            source,
            &[
                "clone",
                "--local",
                "--no-hardlinks",
                "--quiet",
                &source.to_string_lossy(),
                &clone_source.to_string_lossy(),
            ],
        )?;
    }
    git(
        clone_source,
        &["branch", "--force", base_branch, base_commit],
    )?;
    git(
        clone_source,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            &path.to_string_lossy(),
            base_commit,
        ],
    )?;
    // An explicit local identity keeps a worker commit deterministic
    // regardless of host Git identity. A linked worktree reads and writes the
    // clone's own config (no `extensions.worktreeConfig` here), so one write
    // covers both, and none is needed once the clone has it. Only the clone's
    // own file counts: a global identity is what this one overrides.
    let identity = git(
        clone_source,
        &["config", "--local", "--get-regexp", r"^user\.(email|name)$"],
    )
    .unwrap_or_default();
    let mut identity = identity.lines().map(str::trim).collect::<Vec<_>>();
    identity.sort_unstable();
    let expected = [
        format!("user.email {WORKER_EMAIL}"),
        format!("user.name {WORKER_NAME}"),
    ];
    if identity
        .iter()
        .copied()
        .ne(expected.iter().map(String::as_str))
    {
        git(clone_source, &["config", "user.email", WORKER_EMAIL])?;
        git(clone_source, &["config", "user.name", WORKER_NAME])?;
    }
    Ok(())
}

/// Uncommitted and untracked paths reported by Git for a worktree.
///
/// A rename or copy line reports `old -> new`; both sides are changes to the
/// worktree and both must be inside the worker's scope. Taking only the
/// destination would let a worker move a file out of another scope.
fn pending_changes(root: &Path) -> Result<ChangeSet, OrchestratorError> {
    let status = git(root, &["status", "--porcelain=v1", "--untracked-files=all"])?;
    Ok(ChangeSet {
        paths: parse_status_paths(&status),
        insertions: 0,
        deletions: 0,
    })
}

/// Every path a `git status --porcelain=v1` or `git diff --name-status` body
/// names, including both sides of a rename or copy.
fn parse_status_paths(status: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in status.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let payload = line.get(3..).unwrap_or("").trim();
        let (old, new) = match payload.split_once(" -> ") {
            Some((old, new)) => (Some(unquote_path(old)), unquote_path(new)),
            None => (None, unquote_path(payload)),
        };
        if let Some(old) = old
            && !old.is_empty()
        {
            paths.push(old);
        }
        if !new.is_empty() {
            paths.push(new);
        }
    }
    paths
}

/// Strip the quoting Git applies to paths with special characters.
fn unquote_path(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        return trimmed[1..trimmed.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    trimmed.to_owned()
}

/// The staged change set, both sides of a rename or copy included.
///
/// `--name-status` is tab-separated: the first field is the status (possibly
/// `R100`/`C75`) and every following field is a path.
fn staged_changes(root: &Path) -> Result<ChangeSet, OrchestratorError> {
    let body = git(root, &["diff", "--cached", "--name-status"])?;
    let mut paths = Vec::new();
    for line in body.lines() {
        let mut fields = line.split('\t');
        let Some(status) = fields.next() else {
            continue;
        };
        if status.trim().is_empty() {
            continue;
        }
        for field in fields {
            let path = unquote_path(field);
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }
    Ok(ChangeSet {
        paths,
        insertions: 0,
        deletions: 0,
    })
}

/// A path is inside the write scope when it equals a scope entry or lives under
/// a scope entry that names a directory.
#[must_use]
pub fn scope_violations(paths: &[String], write_scope: &[String]) -> Vec<ScopeViolation> {
    let mut violations = Vec::new();
    for path in paths {
        let normalized = path.replace('\\', "/");
        let allowed = write_scope.iter().any(|scope| {
            let scope = scope.trim_end_matches('/');
            scope == "." || normalized == scope || normalized.starts_with(&format!("{scope}/"))
        });
        if !allowed {
            violations.push(ScopeViolation {
                path: normalized,
                reason: format!("outside declared write scope {write_scope:?}"),
            });
        }
    }
    violations
}

fn parse_shortstat(stats: &str) -> (u64, u64) {
    let mut insertions = 0;
    let mut deletions = 0;
    for part in stats.split(',') {
        let part = part.trim();
        let number = part
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        if part.contains("insertion") {
            insertions = number;
        } else if part.contains("deletion") {
            deletions = number;
        }
    }
    (insertions, deletions)
}

pub(crate) fn fingerprint_locked(root: &Path) -> Result<ContentHash, OrchestratorError> {
    let (head, listing) = std::thread::scope(|scope| {
        let listing = scope.spawn(|| list_files(root));
        (git(root, &["rev-parse", "HEAD"]), join_git(listing))
    });
    fingerprint_of(root, head?.trim(), &listing?)
}

/// Every tracked or untracked-but-not-ignored file, sorted.
fn list_files(root: &Path) -> Result<Vec<String>, OrchestratorError> {
    let listing = git(
        root,
        &["ls-files", "--cached", "--others", "--exclude-standard"],
    )?;
    let mut entries = listing
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

/// HEAD plus every listed file's content hash, hashed. Files are hashed on a
/// few threads, and a file whose length and modification time match an
/// earlier hash of it reuses that hash.
fn fingerprint_of(
    root: &Path,
    head: &str,
    entries: &[String],
) -> Result<ContentHash, OrchestratorError> {
    let threads = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, 8);
    let chunk = entries.len().div_ceil(threads).max(1);
    let digests = std::thread::scope(|scope| {
        let workers = entries
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|relative| file_digest(&root.join(relative)))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Vec<_>>();
        let mut digests = Vec::with_capacity(entries.len());
        for worker in workers {
            digests.extend(join_git(worker)?);
        }
        Ok::<_, OrchestratorError>(digests)
    })?;
    let mut records = vec![format!("head\u{0}{head}")];
    for (relative, digest) in entries.iter().zip(&digests) {
        // A file listed by Git and deleted before it was hashed leaves no
        // record: the next listing will not carry it either, so the two
        // fingerprints agree once the deletion has settled.
        if let Some(digest) = digest {
            records.push(format!("{relative}\u{0}{}", digest.as_str()));
        }
    }
    let joined = records.join("\n");
    Ok(ContentHash::from_bytes(joined.as_bytes()))
}

/// A file hash taken earlier: valid while the file keeps its length and
/// modification time.
struct CachedDigest {
    len: u64,
    modified: SystemTime,
    digest: ContentHash,
}

/// How long after its modification time a file's hash may be cached. A write
/// in the same clock tick as the hash could keep both its length and its time,
/// so - as Git treats a "racily clean" index entry - a recent file is always
/// hashed again.
const RACY_WINDOW: Duration = Duration::from_secs(3);
/// More entries than this and the cache starts over, so a huge tree cannot
/// grow it without bound.
const DIGEST_CACHE_LIMIT: usize = 500_000;

fn digest_cache() -> &'static StdMutex<HashMap<PathBuf, CachedDigest>> {
    static CACHE: OnceLock<StdMutex<HashMap<PathBuf, CachedDigest>>> = OnceLock::new();
    CACHE.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// `Ok(None)` when the file is gone: it was listed, then another process
/// deleted it before it could be read. Losing that race is not a failure of
/// the turn - the file simply is not part of the workspace any more.
fn file_digest(absolute: &Path) -> Result<Option<ContentHash>, OrchestratorError> {
    let unreadable = |error: std::io::Error| {
        OrchestratorError::new(
            ErrorCode::StorageOpenFailed,
            format!("cannot read workspace file {}: {error}", absolute.display()),
        )
    };
    // Stream the file: a fingerprint must not load a whole tree into memory,
    // and an unreadable file is a typed failure, not silently an empty file.
    let mut file = match std::fs::File::open(absolute) {
        Ok(file) => file,
        Err(error) if vanished(&error) => return Ok(None),
        Err(error) => return Err(unreadable(error)),
    };
    let stamp = file
        .metadata()
        .ok()
        .and_then(|metadata| Some((metadata.len(), metadata.modified().ok()?)));
    if let Some((len, modified)) = stamp
        && let Ok(cache) = digest_cache().lock()
        && let Some(cached) = cache.get(absolute)
        && cached.len == len
        && cached.modified == modified
    {
        return Ok(Some(cached.digest.clone()));
    }
    let digest = match ContentHash::from_reader(&mut file) {
        Ok(digest) => digest,
        Err(error) if vanished(&error) => return Ok(None),
        Err(error) => {
            return Err(OrchestratorError::new(
                ErrorCode::StorageOpenFailed,
                format!("cannot hash workspace file {}: {error}", absolute.display()),
            ));
        }
    };
    if let Some((len, modified)) = stamp
        && modified.elapsed().is_ok_and(|age| age >= RACY_WINDOW)
        && let Ok(mut cache) = digest_cache().lock()
    {
        if cache.len() >= DIGEST_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(
            absolute.to_path_buf(),
            CachedDigest {
                len,
                modified,
                digest: digest.clone(),
            },
        );
    }
    Ok(Some(digest))
}

/// Whether an I/O failure means the file is no longer where the listing saw it.
///
/// Windows reports a path whose parent went away as `ERROR_PATH_NOT_FOUND` (3)
/// rather than `ERROR_FILE_NOT_FOUND` (2), and only the latter is mapped onto
/// `ErrorKind::NotFound`, so both numbers are matched as well.
fn vanished(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::NotFound {
        return true;
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(2 | 3))
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn git(root: &Path, arguments: &[&str]) -> Result<String, OrchestratorError> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ProcessCanceled,
                format!("cannot run git {}: {error}", arguments.join(" ")),
            )
        })?;
    if !output.status.success() {
        return Err(OrchestratorError::new(
            ErrorCode::StorageWriteFailed,
            format!(
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Canonicalise a repository root into a stable registration key.
fn canonical_key(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

fn git_ok(root: &Path, arguments: &[&str]) -> bool {
    git(root, arguments).is_ok()
}

#[cfg(test)]
mod scope_tests {
    use super::scope_violations;

    #[test]
    fn repository_root_scope_allows_every_relative_path() {
        assert!(
            scope_violations(
                &["src/main.rs".to_owned(), "README.md".to_owned()],
                &[".".to_owned()]
            )
            .is_empty()
        );
    }

    /// A cached hash is reused only for the same length and time; an edit of
    /// the same length moves the time, and a fresh file is never cached.
    #[test]
    fn a_cached_file_hash_follows_every_edit() {
        use std::time::{Duration, SystemTime};

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("a.txt");
        let write = |body: &str, age: Option<u64>| {
            std::fs::write(&path, body).expect("written");
            if let Some(age) = age {
                let file = std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .expect("opened");
                file.set_modified(SystemTime::now() - Duration::from_secs(age))
                    .expect("dated");
            }
        };
        write("one", Some(60));
        let first = super::file_digest(&path).expect("hashed").expect("present");
        assert_eq!(
            super::file_digest(&path).expect("hashed").expect("present"),
            first
        );
        write("two", Some(30));
        let second = super::file_digest(&path).expect("hashed").expect("present");
        assert_ne!(second, first, "same length, new time: hashed again");
        write("one", None);
        assert_eq!(
            super::file_digest(&path).expect("hashed").expect("present"),
            first
        );
        write("six", None);
        assert_ne!(
            super::file_digest(&path).expect("hashed").expect("present"),
            first
        );
    }
}
