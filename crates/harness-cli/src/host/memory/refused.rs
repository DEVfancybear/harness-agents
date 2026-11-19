//! The ledger of memory and prompt-note edits the host refused
//! (`docs/architecture/SELF_IMPROVEMENT_GATE.en.md`).
//!
//! After GUARD (arXiv 2611.23476): a self-improvement loop keeps a ledger of
//! the edits it rejected and reads it back every round, so it does not propose
//! them again. A review's memory or prompt edit that the host refused (a
//! secret, a title too long, a quote the conversation does not hold, the
//! markdown store) used to leave its reason only in the refinement's record
//! and the diagnostics, so the next review planned the same edit again. Each
//! scope keeps its last [`KEPT`] refusals beside its state (`refused.json`),
//! and every planner that proposes memory edits is shown them
//! ([`part`]). An edit that was skipped (the entry changed meanwhile) or that
//! waits for the person is not a refusal and is not kept.

use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

use serde_json::Value;

/// The ledger of one scope, in its folder.
pub const FILE: &str = "refused.json";
/// The lock every process that changes a ledger holds.
const LOCK_FILE: &str = "refused.lock";
/// How long a writer waits for another process holding the lock.
const LOCK_WAIT: Duration = Duration::from_secs(5);
/// How many refusals one scope keeps, the newest (GUARD's `L_max`).
pub const KEPT: usize = 12;
/// The longest a reason, a summary or a quote is kept.
const REASON_CHARS: usize = 300;
const SUMMARY_CHARS: usize = 200;
const EVIDENCE_CHARS: usize = 200;

/// One edit the host refused.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct Refusal {
    /// RFC 3339.
    pub at: String,
    /// `memory` or `prompt`.
    pub kind: String,
    pub action: String,
    /// The entry's id, else its title or path.
    pub target: String,
    pub reason: String,
    /// What it proposed, in a line: title and start of content, redacted.
    pub summary: String,
    /// The quote it rested on, bounded and redacted.
    pub evidence: String,
}

#[derive(Default, serde::Deserialize, serde::Serialize)]
struct Ledger {
    #[serde(default)]
    refused: Vec<Refusal>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The ledger in `dir`. No file is an empty ledger; one that cannot be read or
/// parsed is an error, and nothing writes over it, as with the memory queue.
fn load(dir: &Path) -> Result<Ledger, String> {
    let path = dir.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| {
            format!(
                "the refusal ledger {} cannot be read: {error}",
                path.display()
            )
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Ledger::default()),
        Err(error) => Err(format!(
            "the refusal ledger {} cannot be read: {error}",
            path.display()
        )),
    }
}

/// The refusals kept in `dir`, oldest first; none from an unreadable ledger.
#[must_use]
pub fn refusals(dir: &Path) -> Vec<Refusal> {
    load(dir).map(|ledger| ledger.refused).unwrap_or_default()
}

/// Whether `record`, an edit as a refinement recorded it, is one the host
/// refused: a memory or prompt edit not applied, with an error, that was
/// neither skipped (it was not wrong, something stood in its way) nor put in
/// the queue to wait for the person.
#[must_use]
pub fn is_refusal(record: &Value) -> bool {
    matches!(record["kind"].as_str(), Some("memory" | "prompt"))
        && record["applied"] == false
        && record["skipped"] != true
        && record["stale"] != true
        && !record["queued"].is_string()
        && record["error"]
            .as_str()
            .is_some_and(|error| !error.trim().is_empty())
}

fn refusal_of(record: &Value) -> Refusal {
    let text = |field: &str| record[field].as_str().unwrap_or_default().trim();
    let target = [text("id"), text("title"), text("path")]
        .into_iter()
        .find(|target| !target.is_empty())
        .unwrap_or_default();
    let summary = match (text("title"), text("content")) {
        (title, "") => title.to_owned(),
        ("", content) => content.to_owned(),
        (title, content) => format!("{title}: {content}"),
    };
    Refusal {
        at: now(),
        kind: text("kind").to_owned(),
        action: text("action").to_owned(),
        target: super::learned::one_line(target, 120),
        reason: super::learned::one_line(text("error"), REASON_CHARS),
        summary: super::learned::one_line(&summary, SUMMARY_CHARS),
        evidence: super::learned::one_line(text("evidence"), EVIDENCE_CHARS),
    }
}

/// Keep the refusals among `records` (the edits of one refinement of the scope
/// in `dir`): each replaces an older one for the same kind, target and reason,
/// and only the last [`KEPT`] stay. Returns the ones kept. A ledger that
/// cannot be read or written is reported and left as it is: the refusal is
/// still in the refinement's record.
pub fn keep(dir: &Path, records: &[Value]) -> Vec<Refusal> {
    let fresh = records
        .iter()
        .filter(|record| is_refusal(record))
        .map(refusal_of)
        .collect::<Vec<_>>();
    if fresh.is_empty() {
        return fresh;
    }
    let result = (|| {
        let _lock = super::files::hold(&dir.join(LOCK_FILE), LOCK_WAIT, "refusal ledger")?;
        let mut ledger = load(dir)?;
        for refusal in &fresh {
            ledger.refused.retain(|kept| {
                (&kept.kind, &kept.target, &kept.reason)
                    != (&refusal.kind, &refusal.target, &refusal.reason)
            });
            ledger.refused.push(refusal.clone());
        }
        let excess = ledger.refused.len().saturating_sub(KEPT);
        ledger.refused.drain(..excess);
        let text = serde_json::to_string_pretty(&ledger).map_err(|error| error.to_string())?;
        super::files::write_atomic(&dir.join(FILE), &text)
    })();
    match result {
        Ok(()) => {
            for refusal in &fresh {
                harness_types::diag::info("refine", "refusal_kept")
                    .msg(format!(
                        "a {} edit of {} is refused and kept: {}",
                        refusal.kind, refusal.target, refusal.reason
                    ))
                    .field("kind", refusal.kind.as_str())
                    .field("action", refusal.action.as_str())
                    .field("target", refusal.target.as_str())
                    .field("reason", refusal.reason.as_str())
                    .field("dir", dir.display().to_string())
                    .emit();
            }
            fresh
        }
        Err(error) => {
            harness_types::diag::warn("refine", "refusal_not_kept")
                .field("dir", dir.display().to_string())
                .msg(error)
                .emit();
            Vec::new()
        }
    }
}

/// What a planner is told of the edits the host refused lately in the scopes
/// of `dirs`: not to propose them again unless the conversation shows
/// something the refusal did not know. `None` when there are none.
#[must_use]
pub fn part(dirs: &[&Path]) -> Option<String> {
    let mut all = dirs
        .iter()
        .flat_map(|dir| refusals(dir))
        .collect::<Vec<_>>();
    if all.is_empty() {
        return None;
    }
    all.sort_by(|a, b| b.at.cmp(&a.at));
    all.truncate(KEPT);
    let mut part = "<refused_before>\nThe host refused these memory and prompt edits earlier, with why. Do not propose the same change again unless the conversation shows something the refusal did not know:".to_owned();
    for refusal in all {
        let _ = write!(
            part,
            "\n- {} {} {} `{}`: {}",
            refusal.at.get(..10).unwrap_or(&refusal.at),
            refusal.action,
            refusal.kind,
            refusal.target,
            refusal.reason
        );
        if !refusal.summary.is_empty() {
            let _ = write!(part, " (proposed: {})", refusal.summary);
        }
        if !refusal.evidence.is_empty() {
            let _ = write!(part, " (evidence: {:?})", refusal.evidence);
        }
    }
    part.push_str("\n</refused_before>");
    Some(part)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn refused(kind: &str, id: &str, error: &str) -> Value {
        json!({
            "action": "create", "kind": kind, "id": id, "title": format!("title of {id}"),
            "content": "a fact about the build", "evidence": "the user said so",
            "applied": false, "error": error,
        })
    }

    #[test]
    fn only_a_refusal_with_an_error_is_one() {
        assert!(is_refusal(&refused("memory", "a", "no secrets")));
        assert!(is_refusal(&refused("prompt", "a", "no secrets")));
        // Skipped: it was not wrong.
        let mut skipped = refused("memory", "a", "entry changed");
        skipped["skipped"] = json!(true);
        assert!(!is_refusal(&skipped));
        // Waiting for the person.
        let mut queued = refused("memory", "a", "x");
        queued["queued"] = json!("mq-1");
        assert!(!is_refusal(&queued));
        // Applied, other kinds, no error.
        let mut applied = refused("memory", "a", "x");
        applied["applied"] = json!(true);
        assert!(!is_refusal(&applied));
        assert!(!is_refusal(&refused("skill_file", "a", "x")));
        assert!(!is_refusal(&refused("memory", "a", " ")));
    }

    #[test]
    fn refusals_are_kept_deduplicated_capped_and_redacted() {
        let dir = tempfile::tempdir().expect("dir");
        let mut sensitive = refused("memory", "token", "the harness never keeps secrets");
        sensitive["content"] = json!("the key is sk-abcdefghijklmnopqrstuvwxyz0123456789");
        let kept = keep(dir.path(), &[sensitive, refused("memory", "again", "r")]);
        assert_eq!(kept.len(), 2);
        let ledger = refusals(dir.path());
        assert!(
            ledger[0].summary.contains("REDACTED") && !ledger[0].summary.contains("sk-abc"),
            "{:?}",
            ledger[0]
        );
        // The same kind, target and reason is one record, the newest.
        keep(dir.path(), &[refused("memory", "again", "r")]);
        keep(dir.path(), &[refused("prompt", "again", "r")]);
        let ledger = refusals(dir.path());
        assert_eq!(ledger.len(), 3, "{ledger:?}");
        assert_eq!(ledger[1].kind, "memory");
        assert_eq!(ledger[2].kind, "prompt");
        // The last twelve stay, the oldest dropped.
        for index in 0..20 {
            keep(dir.path(), &[refused("memory", &format!("e{index}"), "r")]);
        }
        let ledger = refusals(dir.path());
        assert_eq!(ledger.len(), KEPT);
        assert_eq!(ledger[0].target, "e8");
        assert_eq!(ledger[KEPT - 1].target, "e19");
    }

    #[test]
    fn a_skipped_or_queued_edit_is_not_kept_and_an_unreadable_ledger_is_not_written_over() {
        let dir = tempfile::tempdir().expect("dir");
        let mut skipped = refused("memory", "a", "entry changed");
        skipped["skipped"] = json!(true);
        let mut queued = refused("memory", "b", "x");
        queued["queued"] = json!("mq-1");
        assert!(keep(dir.path(), &[skipped, queued]).is_empty());
        assert!(!dir.path().join(FILE).exists());
        std::fs::write(dir.path().join(FILE), "not json").expect("write");
        assert!(keep(dir.path(), &[refused("memory", "a", "r")]).is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join(FILE)).expect("read"),
            "not json"
        );
    }

    #[test]
    fn the_part_lists_every_scopes_refusals_newest_first_with_the_instruction() {
        let global = tempfile::tempdir().expect("dir");
        let local = tempfile::tempdir().expect("dir");
        assert!(part(&[global.path(), local.path()]).is_none());
        keep(global.path(), &[refused("memory", "g", "global reason")]);
        keep(local.path(), &[refused("prompt", "l", "local reason")]);
        let told = part(&[global.path(), local.path()]).expect("told");
        assert!(
            told.starts_with("<refused_before>\nThe host refused these"),
            "{told}"
        );
        assert!(
            told.contains("Do not propose the same change again unless the conversation shows something the refusal did not know"),
            "{told}"
        );
        assert!(
            told.contains("`g`: global reason") && told.contains("`l`: local reason"),
            "{told}"
        );
        assert!(
            told.contains("(proposed: title of g: a fact about the build)")
                && told.contains("(evidence: \"the user said so\")"),
            "{told}"
        );
        assert!(told.ends_with("</refused_before>"));
    }
}
