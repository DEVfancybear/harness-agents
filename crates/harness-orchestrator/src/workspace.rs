//! Host-owned isolated worker workspaces.
//!
//! The host owns every shared Git metadata operation. A worker receives one
//! worktree on one branch and never writes to another worker's branch or to the
//! user's checkout. Worktrees isolate concurrent edits; they are explicitly not
//! a security boundary.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex as StdMutex},
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
        if !git_ok(&root, &["rev-parse", "--git-dir"]) {
            return Ok(InputInspection::Dirty(vec![DirtyReason::NotARepository]));
        }
        let mut reasons = Vec::new();
        let status = git(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        )?;
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
            return Ok(InputInspection::Dirty(reasons));
        }
        let head = git(&root, &["rev-parse", "HEAD"])?.trim().to_owned();
        let branch = git(&root, &["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_owned();
        if branch == "HEAD" || branch.is_empty() {
            return Ok(InputInspection::Dirty(vec![DirtyReason::DetachedHead]));
        }
        let fingerprint = fingerprint_locked(&root)?;
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
        fingerprint_locked(&root)
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                OrchestratorError::new(
                    ErrorCode::ArtifactWriteFailed,
                    format!("cannot create worktree parent: {error}"),
                )
            })?;
        }
        let branch = format!("harness/p5/{suffix}");
        let clone_source = self.state_root.join("integration").join("source.git");
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
                &source,
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
        let base_branch = format!("harness/base/{suffix}");
        git(
            &clone_source,
            &["branch", "--force", &base_branch, &snapshot.base_commit],
        )?;
        git(
            &clone_source,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &path.to_string_lossy(),
                &snapshot.base_commit,
            ],
        )?;
        git(
            &clone_source,
            &["config", "user.email", "harness-p5@localhost"],
        )?;
        git(&clone_source, &["config", "user.name", "harness-p5"])?;
        // A worktree inherits configuration, but an explicit local identity
        // keeps a worker commit deterministic regardless of host Git identity.
        git(&path, &["config", "user.email", "harness-p5@localhost"])?;
        git(&path, &["config", "user.name", "harness-p5"])?;
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
            normalized == scope || normalized.starts_with(&format!("{scope}/"))
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

fn fingerprint_locked(root: &Path) -> Result<ContentHash, OrchestratorError> {
    let head = git(root, &["rev-parse", "HEAD"])?.trim().to_owned();
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
    let mut records = vec![format!("head\u{0}{head}")];
    for relative in &entries {
        let absolute = root.join(relative);
        let bytes = std::fs::read(&absolute).unwrap_or_default();
        records.push(format!(
            "{relative}\u{0}{}",
            ContentHash::from_bytes(&bytes).as_str()
        ));
    }
    let joined = records.join("\n");
    Ok(ContentHash::from_bytes(joined.as_bytes()))
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
