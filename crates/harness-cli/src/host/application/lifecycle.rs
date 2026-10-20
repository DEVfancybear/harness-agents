//! The session lifecycle the harness runs, after learn-harness-engineering's
//! lectures 5, 6 and 12: a session starts from what the last one left, and
//! ends leaving something the next one can start from.
//!
//! - Start: the first prompt of a session carries a brief the harness gathers
//!   itself - the latest commits, what is uncommitted, and the handoff the last
//!   session wrote in `.harness/progress.md` - so the agent does not rediscover
//!   the project's state.
//! - Handoff: `/handoff [focus]` has the agent write that file in a fixed shape
//!   (state, verification, decisions, next step); `/handoff --reset` then opens
//!   a fresh conversation that starts from it, the context reset the lectures
//!   recommend when a long conversation is running out of room.
//! - End: leaving checks the workspace and says what would trip the next
//!   session: uncommitted changes, an active feature that is not verified, a
//!   handoff older than the work.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use harness_session::{ContextBlock, ContextBlockKind};

/// The handoff file, relative to the workspace root.
pub const PROGRESS_FILE: &str = ".harness/progress.md";

/// How much of the handoff a session's first prompt carries.
const HANDOFF_BRIEF_BYTES: usize = 6 * 1024;

/// Commits the brief shows.
const BRIEF_COMMITS: usize = 5;

/// Commit subjects and tracked paths the prompt is compared with, to tell a
/// task about the project from a greeting or a general question.
const RELEVANCE_COMMITS: usize = 50;
const RELEVANCE_PATHS: usize = 20_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoffStamp {
    hash: harness_types::ContentHash,
    modified: Option<std::time::SystemTime>,
}

/// A nonempty, readable handoff actually persisted in the workspace.
#[must_use]
pub fn handoff_stamp(root: &Path) -> Option<HandoffStamp> {
    let path = root.join(PROGRESS_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(HandoffStamp {
        hash: harness_types::ContentHash::from_bytes(text.as_bytes()),
        modified: std::fs::metadata(path)
            .ok()
            .and_then(|metadata| metadata.modified().ok()),
    })
}

/// What `/handoff --reset` asks the fresh conversation.
pub const CONTINUE_FROM_HANDOFF: &str = "Continue from the handoff the previous session left in .harness/progress.md (shown in the session brief): check its current state against the repository, then carry on with its next step.";

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned()
    })
}

/// The handoff's text, cut to `limit` bytes on a character boundary.
fn handoff(root: &Path, limit: usize) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(root.join(PROGRESS_FILE)).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    if text.len() <= limit {
        return Some((text, false));
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Some((text[..end].to_owned(), true))
}

/// Whether the prompt is about the project: one of its terms names something
/// the project holds - a tracked path, a recent commit, a pending change, a
/// line of the handoff - and is distinctive there, found in at most half of
/// those documents. Terms come from `harness::query_terms`, so no script or
/// language is special and no word list decides; a greeting shares no
/// distinctive term with the project, a task names what it is about.
fn concerns_project(prompt: &str, documents: &[&str]) -> bool {
    let terms = super::harness::query_terms(prompt);
    if terms.is_empty() || documents.is_empty() {
        return false;
    }
    let mut matches = vec![0_usize; terms.len()];
    for document in documents {
        let words = super::harness::query_terms(document)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        for (count, term) in matches.iter_mut().zip(&terms) {
            if words.contains(term) {
                *count += 1;
            }
        }
    }
    matches
        .into_iter()
        .any(|count| count > 0 && 2 * count <= documents.len())
}

/// The brief a session's first prompt carries, when there is anything to say.
/// It asks for a baseline check outright when the prompt concerns the project,
/// and leaves the call to the model otherwise.
#[must_use]
pub fn startup_brief(root: &Path, prompt: &str) -> Option<ContextBlock> {
    let mut text = String::new();
    // The reads are independent; each is a process start on Windows, so they
    // run side by side.
    let (log, status, files) = std::thread::scope(|scope| {
        let status = scope.spawn(|| git(root, &["--no-optional-locks", "status", "--short"]));
        let files = scope.spawn(|| git(root, &["ls-files", "-z"]));
        let log = git(
            root,
            &["log", "--format=%h %s", &format!("-{RELEVANCE_COMMITS}")],
        );
        (
            log,
            status.join().ok().flatten(),
            files.join().ok().flatten(),
        )
    });
    let commits = log
        .as_deref()
        .map(|log| log.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    if !commits.is_empty() {
        let shown = commits
            .iter()
            .take(BRIEF_COMMITS)
            .copied()
            .collect::<Vec<_>>();
        let _ = write!(text, "Recent commits:\n{}\n", shown.join("\n"));
    }
    let mut documents = commits
        .iter()
        .map(|line| line.split_once(' ').map_or(*line, |(_, subject)| subject))
        .collect::<Vec<_>>();
    documents.extend(
        files
            .as_deref()
            .unwrap_or_default()
            .split('\0')
            .filter(|path| !path.is_empty())
            .take(RELEVANCE_PATHS),
    );
    if let Some(status) = &status {
        let lines = status.lines().collect::<Vec<_>>();
        documents.extend(lines.iter().map(|line| line.get(3..).unwrap_or(line)));
        if lines.is_empty() {
            text.push_str("The worktree is clean.\n");
        } else {
            let _ = write!(
                text,
                "Uncommitted ({} file(s)):\n{}{}\n",
                lines.len(),
                lines
                    .iter()
                    .take(15)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n"),
                if lines.len() > 15 { "\n..." } else { "" }
            );
        }
    }
    let handoff = handoff(root, HANDOFF_BRIEF_BYTES);
    if let Some((handoff, _)) = &handoff {
        documents.extend(handoff.lines().filter(|line| !line.trim().is_empty()));
    }
    let baseline = concerns_project(prompt, &documents);
    if let Some((handoff, cut)) = handoff {
        let _ = write!(
            text,
            "\nHandoff from the last session ({PROGRESS_FILE}):\n{}{}\n",
            handoff.trim_end(),
            if cut {
                "\n[cut: read the file for the rest]"
            } else {
                ""
            }
        );
    }
    if text.is_empty() {
        return None;
    }
    text.insert_str(
        0,
        "Session start - the harness gathered where the project stands, so you need not rediscover it:\n\n",
    );
    // A shared term settles it; without one the prompt may still be a task
    // written in other words or another language than the project's, so the
    // model, which reads any language, decides.
    if baseline {
        text.push_str("\nBefore new work, check that the starting state is sound (the project's checks pass); fix a broken baseline first instead of building on it.");
    } else {
        text.push_str("\nIf this request asks you to change the project, first check that the starting state is sound (the project's checks pass) and fix a broken baseline before building on it. If it is a greeting or a question, answer it directly without running the checks.");
    }
    Some(ContextBlock::mandatory(
        "session-start",
        ContextBlockKind::Instruction,
        text,
    ))
}

/// What `/handoff [focus]` asks the agent.
#[must_use]
pub fn handoff_request(focus: Option<&str>) -> String {
    let focus = focus
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .map(|focus| format!("\n\nFocus on: {focus}"))
        .unwrap_or_default();
    format!(
        "Write the session handoff to {PROGRESS_FILE} now. The next session - possibly another agent with none of this conversation - starts from that file alone. Replace the file's \"Current state\" part and keep any older session log below it, adding one entry for this session. Use this shape:\n\n# Progress\n\n## Current state\n- Branch and last commit:\n- Verification: which checks ran and their result (run the project's checks now if you are not sure)\n- Active feature or task:\n\n## Done this session\n## In progress\n## Blocked or known issues\n## Decisions (what was chosen, and why the alternatives were not)\n## Next step (one concrete action)\n## Commands (start, verify, a focused debug command)\n\n## Session log\n- <date>: <one line on what this session did>\n\nBe specific - paths, commands, exact error messages - and write nothing you did not verify. Then say in one line what the next step is.{focus}"
    )
}

/// What would trip the next session, one line each; empty when nothing would.
#[must_use]
pub fn clean_state(root: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    // NUL-separated porcelain leaves spaces, Unicode and rename destinations
    // intact. Human-readable status quotes paths and is not a filesystem path.
    let status = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
        ],
    );
    let mut records = status.as_deref().unwrap_or_default().split('\0');
    let mut changed = Vec::new();
    while let Some(record) = records.next() {
        if let Some(path) = record.get(3..) {
            changed.push(path);
            if record.as_bytes()[..2]
                .iter()
                .any(|byte| matches!(byte, b'R' | b'C'))
            {
                // In -z output, the destination precedes the source.
                let _ = records.next();
            }
        }
    }
    if !changed.is_empty() {
        lines.push(format!(
            "{} uncommitted file(s) ({}{}): commit what is safe, or say in the handoff why not",
            changed.len(),
            changed
                .iter()
                .take(4)
                .copied()
                .collect::<Vec<_>>()
                .join(", "),
            if changed.len() > 4 { ", ..." } else { "" }
        ));
    }
    if let Ok(Some(list)) = super::features::load(root) {
        for feature in list
            .features
            .iter()
            .filter(|feature| feature.state == super::features::State::Active)
        {
            lines.push(format!(
                "feature {} is still active and not verified: verify it, block it with a reason, or note it in the handoff",
                feature.id
            ));
        }
    }
    if !changed.is_empty() {
        let handoff = root.join(PROGRESS_FILE);
        let stale = match std::fs::metadata(&handoff).and_then(|meta| meta.modified()) {
            Err(_) => Some(format!("no handoff in {PROGRESS_FILE}")),
            Ok(written) => {
                let newest_change = changed
                    .iter()
                    .filter_map(|path| std::fs::metadata(root.join(path)).ok())
                    .filter_map(|meta| meta.modified().ok())
                    .max();
                newest_change
                    .is_some_and(|change| change > written)
                    .then(|| format!("{PROGRESS_FILE} is older than the latest changes"))
            }
        };
        if let Some(stale) = stale {
            lines.push(format!("{stale}: /handoff writes one for the next session"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::{PROGRESS_FILE, clean_state, handoff_request, startup_brief};

    fn git(root: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "user.email=ha@example.invalid", "-c", "user.name=ha"])
            .args(args)
            .current_dir(root)
            .output()
            .expect("git")
            .status;
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn a_session_starts_from_the_commits_and_the_handoff() {
        let root = tempfile::tempdir().expect("root");
        git(root.path(), &["init", "-q"]);
        std::fs::write(root.path().join("a.txt"), "a").expect("file");
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-q", "-m", "add the parser"]);
        std::fs::create_dir_all(root.path().join(".harness")).expect("dir");
        std::fs::write(
            root.path().join(PROGRESS_FILE),
            "# Progress\n## Next step\nwire the parser into main",
        )
        .expect("handoff");
        let brief = startup_brief(root.path(), "wire the parser")
            .expect("a brief")
            .text;
        assert!(brief.contains("add the parser"), "{brief}");
        assert!(brief.contains("wire the parser into main"), "{brief}");
        assert!(brief.contains("Uncommitted (1 file(s))"), "{brief}");
        assert!(brief.contains("Before new work"), "{brief}");
        let empty = tempfile::tempdir().expect("empty");
        assert!(
            startup_brief(empty.path(), "wire the parser").is_none(),
            "nothing to say"
        );
    }

    #[test]
    fn a_prompt_about_the_project_asks_for_a_baseline_check_outright() {
        let root = tempfile::tempdir().expect("root");
        git(root.path(), &["init", "-q"]);
        std::fs::create_dir_all(root.path().join("src/cart")).expect("dir");
        std::fs::write(root.path().join("src/cart/total.ts"), "x").expect("file");
        std::fs::write(root.path().join("README.md"), "x").expect("file");
        git(root.path(), &["add", "."]);
        git(
            root.path(),
            &["commit", "-q", "-m", "feat(shop): giỏ hàng và voucher"],
        );
        for (prompt, asks) in [
            ("hello bạn", false),
            ("chào bạn, hôm nay thế nào?", false),
            ("😀", false),
            ("fix the cart total", true),
            ("sửa lỗi voucher trong giỏ hàng", true),
            // A task in words the project does not use: the model decides.
            ("thêm tính năng thanh toán", false),
        ] {
            let brief = startup_brief(root.path(), prompt).expect("a brief").text;
            assert!(brief.contains("Recent commits"), "{prompt}: {brief}");
            assert_eq!(brief.contains("Before new work"), asks, "{prompt}: {brief}");
            assert_eq!(
                brief.contains("If this request asks you to change the project"),
                !asks,
                "{prompt}: {brief}"
            );
        }
    }

    #[test]
    fn a_term_in_most_of_the_project_is_not_distinctive() {
        let documents = ["fix the cart", "the docs", "the build", "src/main.rs"];
        assert!(!super::concerns_project("the", &documents));
        assert!(super::concerns_project("the cart", &documents));
        assert!(!super::concerns_project("cart", &[]));
    }

    #[test]
    fn leaving_names_what_would_trip_the_next_session() {
        let root = tempfile::tempdir().expect("root");
        git(root.path(), &["init", "-q"]);
        git(
            root.path(),
            &["commit", "-q", "--allow-empty", "-m", "start"],
        );
        assert!(
            clean_state(root.path()).is_empty(),
            "a clean tree says nothing"
        );
        std::fs::write(root.path().join("half.rs"), "fn x() {}").expect("file");
        std::fs::create_dir_all(root.path().join(".harness")).expect("dir");
        std::fs::write(
            root.path().join(".harness/features.json"),
            r#"{"features": [{"id": "F02", "title": "x", "state": "active"}]}"#,
        )
        .expect("features");
        let lines = clean_state(root.path()).join("\n");
        assert!(lines.contains("uncommitted file(s)"), "{lines}");
        assert!(lines.contains("half.rs"), "{lines}");
        assert!(lines.contains("feature F02 is still active"), "{lines}");
        assert!(lines.contains("no handoff"), "{lines}");
    }

    #[test]
    fn the_handoff_request_names_the_file_and_the_shape() {
        let text = handoff_request(Some("the parser"));
        assert!(text.contains(PROGRESS_FILE));
        assert!(text.contains("## Next step"));
        assert!(text.contains("Focus on: the parser"));
    }

    #[test]
    fn stale_handoff_checks_quoted_paths_and_rename_destinations() {
        use std::fs::File;
        use std::time::{Duration, SystemTime};
        for rename in [false, true] {
            let root = tempfile::tempdir().expect("root");
            git(root.path(), &["init", "-q"]);
            std::fs::create_dir(root.path().join(".harness")).expect("dir");
            let original = "report é.txt";
            std::fs::write(root.path().join(original), "a").expect("file");
            std::fs::write(root.path().join(PROGRESS_FILE), "handoff").expect("handoff");
            git(root.path(), &["add", "."]);
            git(root.path(), &["commit", "-qm", "start"]);
            let written = SystemTime::now() - Duration::from_mins(1);
            File::options()
                .write(true)
                .open(root.path().join(PROGRESS_FILE))
                .expect("open")
                .set_modified(written)
                .expect("mtime");
            if rename {
                git(root.path(), &["mv", original, "renamed é.txt"]);
            } else {
                std::fs::write(root.path().join(original), "changed").expect("edit");
            }
            let warnings = clean_state(root.path()).join("\n");
            assert!(
                warnings.contains("older than the latest changes"),
                "rename={rename}: {warnings}"
            );
            assert!(
                warnings.contains(if rename { "renamed é.txt" } else { original }),
                "{warnings}"
            );
        }
    }
}
