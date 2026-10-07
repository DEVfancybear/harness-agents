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

/// The brief a session's first prompt carries, when there is anything to say.
#[must_use]
pub fn startup_brief(root: &Path) -> Option<ContextBlock> {
    let mut text = String::new();
    if let Some(log) = git(root, &["log", "--oneline", "-5"]).filter(|log| !log.is_empty()) {
        let _ = write!(text, "Recent commits:\n{log}\n");
    }
    if let Some(status) = git(root, &["--no-optional-locks", "status", "--short"]) {
        let lines = status.lines().collect::<Vec<_>>();
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
    if let Some((handoff, cut)) = handoff(root, HANDOFF_BRIEF_BYTES) {
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
    text.push_str("\nBefore new work, check that the starting state is sound (the project's checks pass); fix a broken baseline first instead of building on it.");
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
    let status = git(root, &["--no-optional-locks", "status", "--short"]);
    let changed = status
        .as_deref()
        .map(|status| status.lines().collect::<Vec<_>>())
        .unwrap_or_default();
    if !changed.is_empty() {
        lines.push(format!(
            "{} uncommitted file(s) ({}{}): commit what is safe, or say in the handoff why not",
            changed.len(),
            changed
                .iter()
                .take(4)
                .map(|line| line.get(3..).unwrap_or(line).trim())
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
                    .filter_map(|line| line.get(3..))
                    .filter_map(|path| std::fs::metadata(root.join(path.trim())).ok())
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
        let brief = startup_brief(root.path()).expect("a brief").text;
        assert!(brief.contains("add the parser"), "{brief}");
        assert!(brief.contains("wire the parser into main"), "{brief}");
        assert!(brief.contains("Uncommitted (1 file(s))"), "{brief}");
        let empty = tempfile::tempdir().expect("empty");
        assert!(startup_brief(empty.path()).is_none(), "nothing to say");
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
}
