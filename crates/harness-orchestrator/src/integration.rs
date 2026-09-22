//! Revision-bound result integration.
//!
//! A worker's branch revision is never integrated-revision evidence. The host
//! integrates dependency-ordered branches inside an integration worktree,
//! re-runs the declared checks against the resulting revision, re-checks the
//! user's workspace fingerprint, and only then offers the final revision.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use harness_types::{ContentHash, ErrorCode, ProjectId, TaskId};
use tokio::sync::Mutex;

use crate::contracts::{
    CheckedRevision, IntegrationReport, IntegrationStep, OrchestratorError, VerifiedSnapshot,
};
use crate::workspace::{
    ChangeSet, ScopeViolation, WorkspaceManager, fingerprint_locked, scope_violations,
};

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

    /// Reject a branch whose files fall outside its own declared write scope.
    #[must_use]
    pub fn scope_check(candidate: &BranchCandidate, write_scope: &[String]) -> Vec<ScopeViolation> {
        scope_violations(&candidate.files, write_scope)
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
                match integrate_branch(&root, &clone_source, &candidate.branch) {
                    Ok(()) => {
                        report.steps.push(IntegrationStep {
                            task_id: task_id.clone(),
                            branch: candidate.branch.clone(),
                            result_revision: candidate.result_revision.clone(),
                            applied_commit: head(&root)?,
                        });
                    }
                    Err(error) => {
                        abort(&root);
                        report.conflicts.push(format!("{task_id}: {error}"));
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
        let mut failures = Vec::new();
        for check in checks {
            let outcome = run_check(&root, check);
            let passed = outcome.is_ok();
            report.checks.push(CheckedRevision {
                command: check.clone(),
                revision: report.final_commit.clone(),
                passed,
                artifact_id: None,
            });
            if let Err(error) = outcome {
                failures.push(format!("{check}: {error}"));
            }
        }
        if !failures.is_empty() {
            return Ok(IntegrationOutcome::ChecksFailed {
                report: Box::new(report),
                failures,
            });
        }
        Ok(IntegrationOutcome::Ready(Box::new(report)))
    }

    /// Change set of an integrated branch relative to the snapshot base.
    pub async fn integrated_changes(
        &self,
        report: &IntegrationReport,
    ) -> Result<ChangeSet, OrchestratorError> {
        self.workspace
            .branch_changes(&report.integration_root, &report.base_commit)
            .await
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
        let _ = git(root, &["checkout", "--force", &snapshot.base_commit]);
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
) -> Result<(), OrchestratorError> {
    // Worker branches live in the host-owned clone the worktrees were created
    // from, so that clone is the only fetch source.
    let reference = format!("refs/heads/{branch}");
    let source = clone_source.to_string_lossy().into_owned();
    git(root, &["fetch", "--quiet", &source, &reference])?;
    let message = format!("integrate {branch}");
    let _ = git(
        root,
        &[
            "merge",
            "--no-ff",
            "--no-edit",
            "-m",
            &message,
            "FETCH_HEAD",
        ],
    )?;
    Ok(())
}

fn abort(root: &Path) {
    let _ = git(root, &["merge", "--abort"]);
}

fn head(root: &Path) -> Result<String, OrchestratorError> {
    Ok(git(root, &["rev-parse", "HEAD"])?.trim().to_owned())
}
fn run_check(root: &Path, command: &str) -> Result<(), OrchestratorError> {
    let (program, arguments) = split_command(command);
    if program.is_empty() {
        return Err(OrchestratorError::new(
            ErrorCode::InvalidPayload,
            "an integration check requires a command",
        ));
    }
    let output = Command::new(&program)
        .args(&arguments)
        .current_dir(root)
        .output()
        .map_err(|error| {
            OrchestratorError::new(
                ErrorCode::ProcessCanceled,
                format!("cannot run check {command}: {error}"),
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(OrchestratorError::new(
            ErrorCode::ProcessOutcomeUnknown,
            format!(
                "check {command} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ))
    }
}

/// Split a check string into a program and arguments without a shell.
#[must_use]
pub fn split_command(command: &str) -> (String, Vec<String>) {
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or_default().to_owned();
    let arguments = parts.map(str::to_owned).collect();
    (program, arguments)
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
