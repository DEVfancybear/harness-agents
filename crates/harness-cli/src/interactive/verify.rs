//! What "done" means for a project, checked by the harness rather than claimed
//! by the model.
//!
//! `[verify]` lists the project's checks (`[[verify.checks]]`: a name, a shell
//! command, a hint on how to fix a failure). The harness runs them itself, in
//! order, stopping at the first failure: when the model calls `goal_complete`,
//! when a feature asks to move to passing, as the default gates of
//! `/autonomous`, and on `/verify`. A failure goes back to the agent as what
//! failed, its output and how to fix it, so the agent corrects itself instead
//! of being told only "it's wrong".
//!
//! After the checks pass, an independent verifier - a child agent with a fresh
//! context, read-only tools and a checker's instructions - judges a goal before
//! it completes: the agent that did the work does not grade it.

use std::fmt::Write as _;
use std::path::Path;

use harness_providers::CancellationToken;

/// The time limit of one check when neither it nor `[verify]` names one.
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

/// How much of a failing check's output reaches the agent: the start says what
/// ran, the end usually holds the error.
const OUTPUT_HEAD_CHARS: usize = 1_500;
const OUTPUT_TAIL_CHARS: usize = 4_500;

/// One check of `[[verify.checks]]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Check {
    pub name: String,
    pub command: String,
    pub hint: Option<String>,
    pub timeout_ms: u64,
}

/// `[verify]` as the layers resolved it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Verify {
    pub checks: Vec<Check>,
    /// Whether an independent verifier judges a goal before it completes.
    pub judge: bool,
}

impl Default for Verify {
    fn default() -> Self {
        Self {
            checks: Vec::new(),
            judge: true,
        }
    }
}

impl Verify {
    /// Apply one layer's `[verify]`: a layer that lists checks replaces the
    /// earlier list as a whole, so a project never inherits half of another list.
    pub fn apply(&mut self, section: &harness_types::VerifyConfigV2) {
        if !section.checks.is_empty() {
            let default_seconds = section.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
            self.checks = section
                .checks
                .iter()
                .map(|check| Check {
                    name: check.name.trim().to_owned(),
                    command: check.command.trim().to_owned(),
                    hint: check
                        .hint
                        .as_deref()
                        .map(str::trim)
                        .filter(|hint| !hint.is_empty())
                        .map(str::to_owned),
                    timeout_ms: check
                        .timeout_seconds
                        .unwrap_or(default_seconds)
                        .saturating_mul(1000),
                })
                .collect();
        }
        if let Some(judge) = section.judge {
            self.judge = judge;
        }
    }

    /// The checks as one line for `/config`.
    #[must_use]
    pub fn describe(&self) -> String {
        if self.checks.is_empty() {
            return "none".to_owned();
        }
        self.checks
            .iter()
            .map(|check| check.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// How one check ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckRun {
    pub name: String,
    pub command: String,
    pub passed: bool,
    /// `exited 0`, `exited 101`, `timed out`, ...
    pub exit_text: String,
    pub output: String,
    pub hint: Option<String>,
    pub elapsed_ms: u64,
}

/// The checks of one verification, up to the first failure.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub runs: Vec<CheckRun>,
    /// How many checks there were, run or not.
    pub total: usize,
}

impl Report {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.runs.iter().all(|run| run.passed) && self.runs.len() == self.total
    }

    #[must_use]
    pub fn failure(&self) -> Option<&CheckRun> {
        self.runs.iter().find(|run| !run.passed)
    }

    /// The names of the checks that passed, as `types, tests`.
    #[must_use]
    pub fn passed_names(&self) -> String {
        self.runs
            .iter()
            .filter(|run| run.passed)
            .map(|run| run.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One line of evidence for a record: which checks passed, when.
    #[must_use]
    pub fn evidence(&self) -> String {
        if self.total == 0 {
            return "no project checks are configured".to_owned();
        }
        format!(
            "{} check(s) passed: {}",
            self.runs.len(),
            self.runs
                .iter()
                .map(|run| format!("{} (`{}`)", run.name, run.command))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    /// The failure as the agent should read it: what failed, why (its output),
    /// and how to fix it.
    #[must_use]
    pub fn failure_text(&self) -> Option<String> {
        let failure = self.failure()?;
        let position = self.runs.len();
        let mut text = format!(
            "Verification failed at check `{}` ({position} of {}): `{}` {}.",
            failure.name, self.total, failure.command, failure.exit_text
        );
        let passed = self.passed_names();
        if !passed.is_empty() {
            let _ = write!(text, " Passed before it: {passed}.");
        }
        if failure.output.is_empty() {
            text.push_str("\nThe check printed nothing.");
        } else {
            let _ = write!(text, "\n\nOutput:\n{}", failure.output);
        }
        let _ = write!(
            text,
            "\n\nHow to fix: {}",
            failure.hint.as_deref().unwrap_or(
                "find the cause in the output above, fix it, then verify again. Do not weaken, skip or delete the check or its tests to make it pass."
            )
        );
        Some(text)
    }

    /// The report for the user, one line per check.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        if self.total == 0 {
            return vec![
                "no checks: add [[verify.checks]] (name, command, hint) to .harness/config.toml"
                    .to_owned(),
            ];
        }
        let mut lines = self
            .runs
            .iter()
            .map(|run| {
                format!(
                    "{} {}  `{}`  {} in {}",
                    if run.passed { "✓" } else { "✗" },
                    run.name,
                    run.command,
                    run.exit_text,
                    super::view::clock_label(std::time::Duration::from_millis(run.elapsed_ms))
                )
            })
            .collect::<Vec<_>>();
        if self.runs.len() < self.total {
            lines.push(format!(
                "  {} later check(s) not run after the failure",
                self.total - self.runs.len()
            ));
        }
        if let Some(failure) = self.failure() {
            if !failure.output.is_empty() {
                lines.push(String::new());
                lines.extend(failure.output.lines().map(|line| format!("  {line}")));
            }
            if let Some(hint) = &failure.hint {
                lines.push(String::new());
                lines.push(format!("how to fix: {hint}"));
            }
        }
        lines
    }
}

/// Keep the start and the end of a long output.
fn head_and_tail(output: &str) -> String {
    let count = output.chars().count();
    if count <= OUTPUT_HEAD_CHARS + OUTPUT_TAIL_CHARS {
        return output.to_owned();
    }
    let head = output.chars().take(OUTPUT_HEAD_CHARS).collect::<String>();
    let tail = output
        .chars()
        .skip(count - OUTPUT_TAIL_CHARS)
        .collect::<String>();
    format!(
        "{head}\n... [{} characters omitted] ...\n{tail}",
        count - OUTPUT_HEAD_CHARS - OUTPUT_TAIL_CHARS
    )
}

/// Run `checks` in `root`, in order, stopping at the first failure. A check
/// runs in the shell and scrubbed environment the model's shell tool gets.
pub async fn run(root: &Path, checks: &[Check], cancellation: &CancellationToken) -> Report {
    let mut report = Report {
        runs: Vec::new(),
        total: checks.len(),
    };
    for check in checks {
        let started = std::time::Instant::now();
        let result = harness_tools::run_user_command(
            root,
            &check.command,
            check.timeout_ms,
            cancellation.clone(),
        )
        .await;
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (passed, exit_text, output) = match result {
            Ok(done) => {
                let passed = done.status == harness_tools::HookProcessStatus::Exited
                    && done.exit_code == Some(0);
                let exit_text = match done.status {
                    harness_tools::HookProcessStatus::TimedOut => format!(
                        "timed out after {}",
                        super::view::clock_label(std::time::Duration::from_millis(
                            check.timeout_ms
                        ))
                    ),
                    harness_tools::HookProcessStatus::Canceled => "was canceled".to_owned(),
                    harness_tools::HookProcessStatus::Exited => done
                        .exit_code
                        .map_or_else(|| "exited".to_owned(), |code| format!("exited {code}")),
                };
                let mut output = [done.stdout.trim_end(), done.stderr.trim_end()]
                    .iter()
                    .filter(|part| !part.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                if done.stdout_truncated || done.stderr_truncated {
                    output.push_str("\n... [output truncated by the process spool]");
                }
                (passed, exit_text, head_and_tail(output.trim()))
            }
            Err(error) => (false, format!("could not start ({error})"), String::new()),
        };
        report.runs.push(CheckRun {
            name: check.name.clone(),
            command: check.command.clone(),
            passed,
            exit_text,
            // A passing check's output is not evidence anyone reads.
            output: if passed { String::new() } else { output },
            hint: check.hint.clone(),
            elapsed_ms,
        });
        if !passed {
            break;
        }
    }
    report
}

/// The verifier's verdict, from the last `VERDICT:` line of its answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verdict {
    Pass,
    Fail,
    /// The answer has no verdict line: it counts as not verified.
    Missing,
}

#[must_use]
pub fn verdict(answer: &str) -> Verdict {
    for line in answer.lines().rev() {
        let line = line
            .trim()
            .trim_matches(|char| matches!(char, '*' | '`' | '#' | ' '));
        let upper = line.to_ascii_uppercase();
        if let Some(rest) = upper.strip_prefix("VERDICT:") {
            return match rest.trim() {
                "PASS" => Verdict::Pass,
                "FAIL" => Verdict::Fail,
                _ => Verdict::Missing,
            };
        }
    }
    Verdict::Missing
}

/// The instructions of a verifier child: a checker, not a maker.
pub const VERIFIER_POLICY: &str = "You are an independent verifier spawned by the harness. You did not write the work you are checking, and you must not trust the claim that it is done: your job is to find what is wrong or missing, with evidence. You cannot edit files. Read the changed code, run the commands that show whether it works (tests, the program itself), and compare the result against every requirement of the task. List each problem with its file:line and the evidence. Finding nothing because you looked at too little is a failure of your job; passing work that is incomplete is worse than failing work that is done. End your answer with exactly one line: `VERDICT: PASS` when every requirement is met and verified, otherwise `VERDICT: FAIL`.";

/// The brief a verifier gets to judge a goal.
#[must_use]
pub fn goal_brief(objective: &str, claim: &str, checks: &Report, changes: &str) -> String {
    let checks_line = if checks.total == 0 {
        "No project checks are configured; you have to establish yourself whether it works."
            .to_owned()
    } else {
        format!(
            "The harness already ran the project's checks and they passed: {}. Look for what they do not cover.",
            checks.passed_names()
        )
    };
    format!(
        "Judge whether this goal is really achieved.\n\n<goal>\n{objective}\n</goal>\n\n<claim>\nThe working agent says it is done: {claim}\n</claim>\n\n{checks_line}\n\nChanges in the workspace:\n{changes}\n\nCheck every requirement of the goal against the code and its behaviour. End with `VERDICT: PASS` or `VERDICT: FAIL`."
    )
}

/// What changed in the workspace, for a verifier's brief: `git status` and the
/// diff's size, bounded.
pub async fn changes(root: &Path) -> String {
    async fn git(root: &Path, args: &[&str]) -> Option<String> {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned()
        })
    }
    let status = git(root, &["--no-optional-locks", "status", "--short"]).await;
    let stat = git(root, &["--no-optional-locks", "diff", "--stat", "HEAD"]).await;
    let mut text = String::new();
    match status {
        None => text.push_str("(not a git repository: inspect the files the claim names)"),
        Some(status) if status.is_empty() => {
            text.push_str(
                "(no uncommitted changes; the work may be in recent commits: see git log)",
            );
        }
        Some(status) => {
            text.push_str(&status.lines().take(60).collect::<Vec<_>>().join("\n"));
            if let Some(stat) = stat.filter(|stat| !stat.is_empty()) {
                text.push_str("\n\n");
                text.push_str(&stat.lines().take(60).collect::<Vec<_>>().join("\n"));
            }
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{Check, Report, Verdict, Verify, head_and_tail, run, verdict};

    fn check(name: &str, command: &str) -> Check {
        Check {
            name: name.to_owned(),
            command: command.to_owned(),
            hint: Some(format!("fix {name}")),
            timeout_ms: 30_000,
        }
    }

    #[test]
    fn a_layer_with_checks_replaces_the_list_and_keeps_the_judge_default() {
        let mut verify = Verify::default();
        verify.apply(&harness_types::VerifyConfigV2 {
            checks: vec![harness_types::VerifyCheckV2 {
                name: " tests ".to_owned(),
                command: "cargo test".to_owned(),
                hint: Some("  ".to_owned()),
                timeout_seconds: None,
            }],
            timeout_seconds: Some(60),
            judge: None,
        });
        assert_eq!(verify.checks.len(), 1);
        assert_eq!(verify.checks[0].name, "tests");
        assert_eq!(verify.checks[0].timeout_ms, 60_000);
        assert_eq!(verify.checks[0].hint, None);
        assert!(verify.judge);
        verify.apply(&harness_types::VerifyConfigV2 {
            checks: Vec::new(),
            timeout_seconds: None,
            judge: Some(false),
        });
        assert_eq!(
            verify.checks.len(),
            1,
            "an empty list keeps the earlier one"
        );
        assert!(!verify.judge);
    }

    #[tokio::test]
    async fn checks_stop_at_the_first_failure_and_say_how_to_fix_it() {
        let root = tempfile::tempdir().expect("root");
        let checks = [
            check("first", "echo fine"),
            check("second", "echo broken-output; exit 3"),
            check("third", "echo never"),
        ];
        let report = run(
            root.path(),
            &checks,
            &harness_providers::CancellationToken::new(),
        )
        .await;
        assert!(!report.passed());
        assert_eq!(report.runs.len(), 2);
        assert_eq!(report.total, 3);
        let text = report.failure_text().expect("failure");
        assert!(text.contains("check `second` (2 of 3)"), "{text}");
        assert!(text.contains("exited 3"), "{text}");
        assert!(text.contains("broken-output"), "{text}");
        assert!(text.contains("How to fix: fix second"), "{text}");
        assert!(text.contains("Passed before it: first."), "{text}");
        let lines = report.lines().join("\n");
        assert!(lines.contains("1 later check(s) not run"), "{lines}");
    }

    #[tokio::test]
    async fn passing_checks_leave_evidence() {
        let root = tempfile::tempdir().expect("root");
        let report = run(
            root.path(),
            &[check("only", "echo ok")],
            &harness_providers::CancellationToken::new(),
        )
        .await;
        assert!(report.passed());
        assert!(report.failure_text().is_none());
        assert!(report.evidence().contains("only (`echo ok`)"));
        assert!(Report::default().passed(), "no checks is not a failure");
    }

    #[test]
    fn the_verdict_is_the_last_verdict_line() {
        assert_eq!(verdict("looks fine\nVERDICT: PASS"), Verdict::Pass);
        assert_eq!(verdict("**VERDICT: FAIL**\n"), Verdict::Fail);
        assert_eq!(
            verdict("VERDICT: PASS\nlater found a bug\nverdict: fail"),
            Verdict::Fail
        );
        assert_eq!(verdict("it works, trust me"), Verdict::Missing);
        assert_eq!(verdict("VERDICT: maybe"), Verdict::Missing);
    }

    #[test]
    fn long_output_keeps_its_start_and_its_end() {
        let output = format!("{}{}", "a".repeat(5_000), "z".repeat(5_000));
        let kept = head_and_tail(&output);
        assert!(kept.starts_with("aaa"));
        assert!(kept.ends_with("zzz"));
        assert!(kept.contains("characters omitted"));
    }
}
