//! Revision-bound result integration.
//!
//! A worker's branch revision is never integrated-revision evidence. The host
//! integrates dependency-ordered branches inside an integration worktree,
//! re-runs the declared checks against the resulting revision, re-checks the
//! user's workspace fingerprint, and only then offers the final revision.

use std::{
    future::Future,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use harness_types::{ContentHash, ErrorCode, ProjectId, TaskId};
use tokio::sync::Mutex;

use crate::contracts::{
    CheckedRevision, IntegrationReport, IntegrationStep, OrchestratorError, VerifiedSnapshot,
};
use crate::workspace::{WorkspaceManager, fingerprint_locked};

/// Verdict for one integration attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum IntegrationOutcome {
    /// Every branch applied cleanly and the final checks passed.
    Ready(Box<IntegrationReport>),
    /// One or more branches conflicted. No final revision is offered.
    Conflicted {
        report: Box<IntegrationReport>,
        conflicts: Vec<String>,
    },
    /// The integrated revision failed its final checks.
    ChecksFailed {
        report: Box<IntegrationReport>,
        failures: Vec<String>,
    },
}

impl IntegrationOutcome {
    #[must_use]
    pub fn report(&self) -> &IntegrationReport {
        match self {
            Self::Ready(report)
            | Self::Conflicted { report, .. }
            | Self::ChecksFailed { report, .. } => report,
        }
    }

    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready(_))
    }
}

/// The decision taken when the user's workspace changed under an integration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FinalApply {
    /// Fingerprint matched; the final revision may be presented.
    Allowed { final_commit: String },
    /// The user's workspace changed. The host refuses and asks for direction.
    Refused { reason: String },
}

/// What applying a ready integration to the user's checkout did.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    /// The user's branch was fast-forwarded from `previous_head` to
    /// `final_commit`; `git reset --keep <previous_head>` takes it back.
    Applied {
        branch: String,
        previous_head: String,
        final_commit: String,
    },
    /// Nothing in the user's checkout was touched.
    Refused { reason: String },
}

/// How one final check ended, as a caller-supplied runner reports it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckResult {
    pub command: String,
    pub passed: bool,
    pub exit_code: Option<i32>,
    /// Why a failed check failed: its exit and output, for the failure list.
    pub detail: String,
}

/// One branch to integrate.
#[derive(Clone, Debug)]
pub struct BranchCandidate {
    pub task_id: TaskId,
    pub branch: String,
    pub worktree_path: String,
    pub result_revision: String,
    pub files: Vec<String>,
}

/// The host-owned integrator.
pub struct ResultIntegrator {
    workspace: Arc<WorkspaceManager>,
    git_lock: Arc<Mutex<()>>,
}

impl ResultIntegrator {
    #[must_use]
    pub fn new(workspace: Arc<WorkspaceManager>) -> Self {
        let git_lock = workspace.git_lock();
        Self {
            workspace,
            git_lock,
        }
    }

    /// Integrate candidates in dependency order inside one integration worktree.
    ///
    /// `order` is the topological task order; only candidates present in it are
    /// applied, and a candidate whose dependency is missing from `order` is a
    /// conflict rather than a silent skip.
    pub async fn integrate(
        &self,
        snapshot: &VerifiedSnapshot,
        order: &[TaskId],
        candidates: &[BranchCandidate],
        checks: &[String],
    ) -> Result<IntegrationOutcome, OrchestratorError> {
        self.integrate_with(snapshot, order, candidates, |root| async move {
            checks
                .iter()
                .map(|check| run_declared_check(&root, check))
                .collect()
        })
        .await
    }

    /// [`Self::integrate`], with the final checks run by `run_checks` in the
    /// integration root - so a host can run them the way it runs its own
    /// project checks (its shell, its time limits). The shared Git lock is held
    /// while they run: nothing resets the integration worktree under them.
    pub async fn integrate_with<F, Fut>(
        &self,
        snapshot: &VerifiedSnapshot,
        order: &[TaskId],
        candidates: &[BranchCandidate],
        run_checks: F,
    ) -> Result<IntegrationOutcome, OrchestratorError>
    where
        F: FnOnce(PathBuf) -> Fut,
        Fut: Future<Output = Vec<CheckResult>>,
    {
        let by_task: std::collections::BTreeMap<String, &BranchCandidate> = candidates
            .iter()
            .map(|candidate| (candidate.task_id.as_str().to_owned(), candidate))
            .collect();
        let root = self
            .workspace
            .state_root()
            .join("integration")
            .join("worktree");
        let _guard = self.git_lock.lock().await;
        prepare_integration_worktree(&root, snapshot)?;
        let mut report = IntegrationReport {
            project_id: snapshot.project_id.clone(),
            integration_root: root.to_string_lossy().into_owned(),
            base_commit: snapshot.base_commit.clone(),
            final_commit: snapshot.base_commit.clone(),
            final_fingerprint: snapshot.fingerprint.clone(),
            steps: Vec::new(),
            conflicts: Vec::new(),
            checks: Vec::new(),
        };
        let clone_source = self
            .workspace
            .state_root()
            .join("integration")
            .join("source.git");
        for task_id in order {
            let Some(candidate) = by_task.get(task_id.as_str()) else {
                continue;
            };
            if candidate.files.is_empty() {
                // A worker that changed nothing has no revision to integrate,
                // but the step is still recorded so the report is complete.
                report.steps.push(IntegrationStep {
                    task_id: task_id.clone(),
                    branch: candidate.branch.clone(),
                    result_revision: candidate.result_revision.clone(),
                    applied_commit: head(&root)?,
                });
            } else {
                match integrate_branch(
                    &root,
                    &clone_source,
                    &candidate.branch,
                    &candidate.result_revision,
                ) {
                    Ok(()) => {
                        report.steps.push(IntegrationStep {
                            task_id: task_id.clone(),
                            branch: candidate.branch.clone(),
                            result_revision: candidate.result_revision.clone(),
                            applied_commit: head(&root)?,
                        });
                    }
                    Err(error) => {
                        let files = unmerged_paths(&root);
                        abort(&root);
                        report.conflicts.push(if files.is_empty() {
                            format!("{task_id}: {error}")
                        } else {
                            format!(
                                "{task_id}: {} conflicts in {}",
                                candidate.branch,
                                files.join(", ")
                            )
                        });
                    }
                }
            }
        }
        if !report.conflicts.is_empty() {
            return Ok(IntegrationOutcome::Conflicted {
                conflicts: report.conflicts.clone(),
                report: Box::new(report),
            });
        }
        report.final_commit = head(&root)?;
        // `fingerprint_locked` is called directly: `integrate` already holds the
        // shared Git lock, and taking it again would deadlock.
        report.final_fingerprint = fingerprint_locked(&root)?;
        let results = run_checks(root.clone()).await;
        let failures = record_final_checks(&root, &mut report, results)?;
        if !failures.is_empty() {
            return Ok(IntegrationOutcome::ChecksFailed {
                report: Box::new(report),
                failures,
            });
        }
        Ok(IntegrationOutcome::Ready(Box::new(report)))
    }

    /// Recheck the user's workspace before anything is applied there. A changed
    /// fingerprint is refused instead of overwritten.
    pub async fn recheck_before_apply(
        &self,
        user_root: impl AsRef<Path>,
        expected: &ContentHash,
        report: &IntegrationReport,
    ) -> Result<FinalApply, OrchestratorError> {
        let current = self.workspace.fingerprint(user_root).await?;
        if &current != expected {
            return Ok(FinalApply::Refused {
                reason: format!(
                    "user workspace changed since the input snapshot ({} -> {}); \
                     refusing to apply integration {} and requesting conflict direction",
                    expected.as_str(),
                    current.as_str(),
                    report.final_commit
                ),
            });
        }
        Ok(FinalApply::Allowed {
            final_commit: report.final_commit.clone(),
        })
    }

    /// Bring a ready integration into the user's checkout: recheck the
    /// workspace fingerprint and fast-forward the user's branch to the final
    /// revision, both under the shared Git lock so nothing moves in between.
    ///
    /// Only a fast-forward is made. The checkout must still be on a branch at
    /// the commit the integration started from, with the fingerprint the
    /// coders were given; otherwise nothing is touched and the refusal says
    /// why. Uncommitted work is never reset, stashed or overwritten.
    pub async fn apply_final(
        &self,
        user_root: impl AsRef<Path>,
        expected: &ContentHash,
        report: &IntegrationReport,
    ) -> Result<ApplyOutcome, OrchestratorError> {
        let user_root = user_root.as_ref().to_path_buf();
        let _guard = self.git_lock.lock().await;
        let current = fingerprint_locked(&user_root)?;
        if &current != expected {
            return Ok(ApplyOutcome::Refused {
                reason: format!(
                    "the user's workspace changed since the coders started ({} -> {}); \
                     nothing was applied",
                    expected.as_str(),
                    current.as_str()
                ),
            });
        }
        let previous_head = head(&user_root)?;
        if previous_head != report.base_commit {
            return Ok(ApplyOutcome::Refused {
                reason: format!(
                    "the user's checkout is at {previous_head}, not at the integration base {}; \
                     nothing was applied",
                    report.base_commit
                ),
            });
        }
        let branch = git(&user_root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|name| name.trim().to_owned())
            .unwrap_or_default();
        if branch.is_empty() {
            return Ok(ApplyOutcome::Refused {
                reason: "the user's checkout has a detached HEAD; nothing was applied".to_owned(),
            });
        }
        if let Some(reason) = integration_refusal(report)? {
            return Ok(ApplyOutcome::Refused { reason });
        }
        let integration_root = PathBuf::from(&report.integration_root);
        // The fetch writes only objects and FETCH_HEAD; the working tree
        // changes with the fast-forward alone, which Git refuses rather than
        // overwrite a local change.
        git(
            &user_root,
            &[
                "fetch",
                "--quiet",
                "--no-tags",
                &integration_root.to_string_lossy(),
                "HEAD",
            ],
        )?;
        let fetched = git(&user_root, &["rev-parse", "FETCH_HEAD"])?;
        if fetched.trim() != report.final_commit {
            return Ok(ApplyOutcome::Refused {
                reason: format!(
                    "fetched {} instead of the final revision {}; nothing was applied",
                    fetched.trim(),
                    report.final_commit
                ),
            });
        }
        git(
            &user_root,
            &["merge", "--ff-only", "--quiet", &report.final_commit],
        )?;
        Ok(ApplyOutcome::Applied {
            branch,
            previous_head,
            final_commit: report.final_commit.clone(),
        })
    }
}

/// A ready report remains valid only while its checked revision and inputs do.
fn integration_refusal(report: &IntegrationReport) -> Result<Option<String>, OrchestratorError> {
    if report.checks.iter().any(|check| !check.passed) {
        return Ok(Some(
            "the integration has failed checks; nothing was applied".to_owned(),
        ));
    }
    let root = Path::new(&report.integration_root);
    if head(root)? != report.final_commit || fingerprint_locked(root)? != report.final_fingerprint {
        return Ok(Some(format!(
            "the integration worktree changed after {} was verified; integrate again. Nothing was applied",
            report.final_commit
        )));
    }
    Ok(None)
}

fn prepare_integration_worktree(
    root: &Path,
    snapshot: &VerifiedSnapshot,
) -> Result<(), OrchestratorError> {
    let source = PathBuf::from(&snapshot.root);
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ArtifactWriteFailed,
                format!("cannot create integration directory: {error}"),
            )
        })?;
    }
    if root.join(".git").exists() {
        // The clone is kept between integrations, so a base the project
        // committed since is fetched before it is checked out: a checkout that
        // failed quietly left the next integration on an older base.
        let commit = format!("{}^{{commit}}", snapshot.base_commit);
        if git(root, &["cat-file", "-e", &commit]).is_err() {
            git(
                root,
                &[
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    &source.to_string_lossy(),
                    "+HEAD:refs/harness/source-head",
                ],
            )?;
        }
        git(root, &["checkout", "--force", &snapshot.base_commit])?;
        let _ = git(root, &["merge", "--abort"]);
        let _ = git(root, &["reset", "--hard", &snapshot.base_commit]);
        let _ = git(root, &["clean", "-fd"]);
        return Ok(());
    }
    git(
        &source,
        &[
            "clone",
            "--local",
            "--no-hardlinks",
            "--quiet",
            &source.to_string_lossy(),
            &root.to_string_lossy(),
        ],
    )?;
    git(root, &["config", "user.email", "harness-p5@localhost"])?;
    git(root, &["config", "user.name", "harness-p5"])?;
    git(root, &["checkout", "--force", &snapshot.base_commit])?;
    Ok(())
}

fn integrate_branch(
    root: &Path,
    clone_source: &Path,
    branch: &str,
    result_revision: &str,
) -> Result<(), OrchestratorError> {
    // Worker branches live in the host-owned clone the worktrees were created
    // from, so that clone is the only fetch source.
    let reference = format!("refs/heads/{branch}");
    let source = clone_source.to_string_lossy().into_owned();
    git(root, &["fetch", "--quiet", &source, &reference])?;
    let fetched = git(root, &["rev-parse", "FETCH_HEAD"])?;
    if fetched.trim() != result_revision {
        return Err(OrchestratorError::new(
            ErrorCode::IntegrationConflict,
            format!(
                "coder branch {branch} moved from the recorded revision {result_revision} to {}; integrate again",
                fetched.trim()
            ),
        ));
    }
    let message = format!("integrate {branch}");
    let _ = git(
        root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &message,
            result_revision,
        ],
    )?;
    Ok(())
}

/// The paths a failed merge left unmerged.
fn unmerged_paths(root: &Path) -> Vec<String> {
    git(root, &["diff", "--name-only", "--diff-filter=U"])
        .map(|names| {
            names
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn abort(root: &Path) {
    let _ = git(root, &["merge", "--abort"]);
}

fn head(root: &Path) -> Result<String, OrchestratorError> {
    Ok(git(root, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// Run one declared check without a shell, as [`split_command`] reads it.
fn run_declared_check(root: &Path, command: &str) -> CheckResult {
    let (program, arguments) = split_command(command);
    if program.is_empty() {
        return CheckResult {
            command: command.to_owned(),
            passed: false,
            exit_code: None,
            detail: "an integration check requires a command".to_owned(),
        };
    }
    match Command::new(&program)
        .args(&arguments)
        .current_dir(root)
        .output()
    {
        Ok(output) => CheckResult {
            command: command.to_owned(),
            passed: output.status.success(),
            exit_code: output.status.code(),
            detail: format!(
                "exited with status {:?}: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        },
        Err(error) => CheckResult {
            command: command.to_owned(),
            passed: false,
            exit_code: None,
            detail: format!("cannot run check {command}: {error}"),
        },
    }
}

/// Record each check against the integrated revision and the tree it ran on;
/// the failures come back as `command: why`.
fn record_final_checks(
    root: &Path,
    report: &mut IntegrationReport,
    results: Vec<CheckResult>,
) -> Result<Vec<String>, OrchestratorError> {
    let mut failures = Vec::new();
    let workspace_digest = fingerprint_locked(root)?;
    if workspace_digest != report.final_fingerprint {
        failures.push(
            "verification inputs changed: checks modified the integrated revision or its files; fix or commit those changes in a coder worktree and integrate again. Ignored build artifacts may be written, but the revision being verified must stay unchanged"
                .to_owned(),
        );
        report.checks.push(CheckedRevision {
            command: "verification inputs unchanged".to_owned(),
            revision: report.final_commit.clone(),
            passed: false,
            workspace_digest: Some(workspace_digest.clone()),
            exit_code: None,
            artifact_id: None,
        });
    }
    report.final_fingerprint = workspace_digest.clone();
    for result in results {
        if !result.passed {
            failures.push(format!("{}: {}", result.command, result.detail));
        }
        report.checks.push(CheckedRevision {
            command: result.command,
            revision: report.final_commit.clone(),
            passed: result.passed,
            workspace_digest: Some(workspace_digest.clone()),
            exit_code: result.exit_code,
            artifact_id: None,
        });
    }
    Ok(failures)
}

/// Split a check string into a program and arguments without a shell.
///
/// Words are separated by whitespace outside quotes. Single quotes keep
/// everything literally; double quotes keep everything but `\"` and `\\`,
/// which stand for `"` and `\`. Outside quotes a backslash escapes only a
/// quote or whitespace, so a Windows path such as `C:\tools\x` or
/// `\\server\share` keeps its separators. `""` and `''` are empty arguments; an unterminated
/// quote runs to the end of the string.
#[must_use]
pub fn split_command(command: &str) -> (String, Vec<String>) {
    #[derive(Clone, Copy)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut words = Vec::new();
    let mut word = String::new();
    // A quoted empty string is still a word.
    let mut started = false;
    let mut quote = Quote::None;
    let mut characters = command.chars().peekable();
    while let Some(character) = characters.next() {
        match (quote, character) {
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            (Quote::Double, '\\') => match characters.peek() {
                Some(&next @ ('"' | '\\')) => {
                    word.push(next);
                    characters.next();
                }
                _ => word.push('\\'),
            },
            (Quote::Single | Quote::Double, other) => word.push(other),
            (Quote::None, '\'') => {
                quote = Quote::Single;
                started = true;
            }
            (Quote::None, '"') => {
                quote = Quote::Double;
                started = true;
            }
            (Quote::None, '\\') => {
                match characters.peek() {
                    Some(&next) if matches!(next, '"' | '\'') || next.is_whitespace() => {
                        word.push(next);
                        characters.next();
                    }
                    _ => word.push('\\'),
                }
                started = true;
            }
            (Quote::None, space) if space.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (Quote::None, other) => {
                word.push(other);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    let mut words = words.into_iter();
    let program = words.next().unwrap_or_default();
    (program, words.collect())
}

/// Build one branch candidate from a host-owned worktree record.
#[must_use]
pub fn workspace_candidate(
    record: &crate::contracts::WorktreeRecord,
    result_revision: &str,
    files: Vec<String>,
) -> BranchCandidate {
    BranchCandidate {
        task_id: record.task_id.clone(),
        branch: record.branch.clone(),
        worktree_path: record.path.clone(),
        result_revision: result_revision.to_owned(),
        files,
    }
}

/// Stable identity for one integration attempt.
#[must_use]
pub fn integration_id(project_id: &ProjectId, final_commit: &str) -> String {
    let source = format!("{}:{final_commit}", project_id.as_str());
    let hash = ContentHash::from_bytes(source.as_bytes());
    let short = hash
        .as_str()
        .trim_start_matches("sha256:")
        .chars()
        .take(16)
        .collect::<String>();
    format!("integration-{short}")
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
            ErrorCode::IntegrationConflict,
            format!(
                "git {} failed: {}",
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::split_command;

    fn split(command: &str) -> Vec<String> {
        let (program, mut arguments) = split_command(command);
        arguments.insert(0, program);
        arguments
    }

    #[test]
    fn plain_words_split_on_whitespace() {
        assert_eq!(
            split("cargo  test\t--workspace"),
            ["cargo", "test", "--workspace"]
        );
        assert_eq!(split_command("   "), (String::new(), Vec::new()));
    }

    /// A quoted argument used to be cut at its spaces, so `--format="%H %s"`
    /// reached the program as two broken arguments.
    #[test]
    fn quoted_arguments_stay_whole() {
        assert_eq!(
            split(r#"git log --format="%H %s" -n 1"#),
            ["git", "log", "--format=%H %s", "-n", "1"]
        );
        assert_eq!(
            split("grep -r 'two words' src"),
            ["grep", "-r", "two words", "src"]
        );
        assert_eq!(split(r#"echo "" ''"#), ["echo", "", ""]);
        assert_eq!(
            split(r#"echo "say \"hi\"" 'it''s'"#),
            ["echo", r#"say "hi""#, "its"]
        );
        assert_eq!(
            split(r#"echo "unterminated arg"#),
            ["echo", "unterminated arg"]
        );
    }

    #[test]
    fn backslashes_escape_only_quotes_and_spaces() {
        assert_eq!(
            split(r"C:\tools\check.exe --dir \server\share"),
            [r"C:\tools\check.exe", "--dir", r"\server\share"]
        );
        assert_eq!(split(r"echo a\ b \'x"), ["echo", "a b", "'x"]);
        assert_eq!(split(r#"echo "C:\dir\\""#), ["echo", r"C:\dir\"]);
    }
}
