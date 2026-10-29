//! Where things stand: the one rule for which record of the work wins, and
//! which earlier conversations a new one may hear of.
//!
//! Five places keep where a task stands - this conversation's task state, the
//! project's markdown memory, the session logs, `.harness/progress.md` and a
//! compaction summary - and each block that carried one used to state its own
//! order, and they disagreed. [`WHERE_THINGS_STAND`] is the order; the system
//! prompt carries it once per request, where it stays cached, and every other
//! text points to it by its name, "Where things stand", instead of ranking itself.
//!
//! A new conversation hears of an earlier one only when that one was left:
//! a conversation still open - in another pane of this process, or in
//! another live ha - is that conversation's work in progress, not a task to
//! adopt. [`OpenConversation`] keeps the registry of open conversations here
//! and leaves a mark for the other processes; [`is_live`] reads both, and
//! the owed session-log marks that live processes leave.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use harness_types::TaskId;

/// The name the precedence rule goes by; every text that ranks a record
/// points to it by this name.
#[cfg(test)]
pub const RULE_NAME: &str = "Where things stand";

/// The precedence rule, as the system prompt carries it.
pub const WHERE_THINGS_STAND: &str = "# Where things stand\n\nWhen records of the work disagree, the higher one wins:\n1. the user's current message;\n2. this conversation's live task state (task_state);\n3. the project's markdown memory files (memory/, in a project that keeps them);\n4. the newest session log (.harness/sessions);\n5. .harness/progress.md, the handoff;\n6. a compaction summary.\nEverything below 2 is a record of earlier work that may be stale - and so are the memory digest and an earlier conversation's task state or turns, which rank with the session log: check one against the repository before acting on it. A new decision goes to task_state for this task and, in a project with markdown memory, to memory/decisions/.";

/// `prompt` with the precedence rule at its end, once: a prompt that already
/// carries it is left as it is.
pub fn with_rule(prompt: &mut String) {
    if prompt.contains(WHERE_THINGS_STAND) {
        return;
    }
    if !prompt.is_empty() {
        prompt.push_str("\n\n");
    }
    prompt.push_str(WHERE_THINGS_STAND);
}

/// Where a project store keeps the marks of the conversations open in a
/// process.
const OPEN_DIR: &str = "open-conversations";

/// This process, as an open mark names its owner: its id and when it started,
/// so a later process given the same id is not taken for it.
static PROCESS: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{}:{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    )
});

/// The conversations open in this process, each with how many holders keep it.
static OPEN: LazyLock<Mutex<HashMap<String, usize>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The project stores this process has opened a conversation under: where
/// [`is_live`] looks for other processes' marks.
static STORES: LazyLock<Mutex<BTreeSet<PathBuf>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

/// A conversation open in this process, from the first turn a pane or a run
/// gives it until the pane moves to another conversation or goes away.
///
/// While one is held, no other conversation is offered its task state, its
/// session log or its last turns: it is still somebody's work in progress.
/// The mark under the project store says the same to other processes.
#[derive(Debug)]
pub struct OpenConversation {
    task: String,
    mark: PathBuf,
}

impl OpenConversation {
    /// Hold `task` open, under the project store `store_dir`.
    #[must_use]
    pub fn hold(store_dir: &Path, task: &TaskId) -> Self {
        let mark = store_dir.join(OPEN_DIR).join(task.as_str());
        if let Ok(mut stores) = STORES.lock() {
            stores.insert(store_dir.to_path_buf());
        }
        if let Ok(mut open) = OPEN.lock() {
            *open.entry(task.as_str().to_owned()).or_default() += 1;
        }
        if let Some(parent) = mark.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&mark, PROCESS.as_str());
        Self {
            task: task.as_str().to_owned(),
            mark,
        }
    }

    /// The conversation held.
    #[must_use]
    pub fn task(&self) -> &str {
        &self.task
    }
}

impl Drop for OpenConversation {
    fn drop(&mut self) {
        let last = OPEN.lock().is_ok_and(|mut open| {
            let left = open.get_mut(&self.task).map_or(0, |count| {
                *count = count.saturating_sub(1);
                *count
            });
            if left == 0 {
                open.remove(&self.task);
            }
            left == 0
        });
        // Only this process's own mark goes: another process may have opened
        // the conversation since.
        if last && std::fs::read_to_string(&self.mark).is_ok_and(|owner| owner.trim() == *PROCESS) {
            let _ = std::fs::remove_file(&self.mark);
        }
    }
}

/// Whether the owner a mark names is a process still running other than
/// this one.
pub(super) fn owner_is_another_live_process(owner: &str) -> bool {
    if owner == *PROCESS {
        return false;
    }
    owner
        .split_once(':')
        .and_then(|(pid, _)| pid.parse::<u32>().ok())
        .is_some_and(|pid| {
            pid != std::process::id() && super::agents::registry::process_is_alive(pid)
        })
}

/// Whether `task` is a conversation still open: held in this process, with a
/// turn running here, or marked open or owed by another live ha process in
/// one of the project stores this process has opened a conversation under.
/// The turn asking holds its own conversation first, so its project's store
/// is always among them.
#[must_use]
pub fn is_live(task: &TaskId) -> bool {
    if OPEN
        .lock()
        .is_ok_and(|open| open.get(task.as_str()).is_some_and(|count| *count > 0))
        || super::session_log::turn_running_here(task)
    {
        return true;
    }
    let stores = STORES
        .lock()
        .map(|stores| stores.clone())
        .unwrap_or_default();
    stores.iter().any(|store| {
        let mark = store.join(OPEN_DIR).join(task.as_str());
        let open_elsewhere = std::fs::read_to_string(&mark).is_ok_and(|owner| {
            let owner = owner.trim();
            let live = owner_is_another_live_process(owner);
            // A mark a killed process left is cleared, so it is not asked
            // about again; this process's own marks belong to its holders.
            if !live && owner != *PROCESS {
                let _ = std::fs::remove_file(&mark);
            }
            live
        });
        open_elsewhere || super::session_log::owed_by_another_live_process(store, task)
    })
}

#[cfg(test)]
mod tests {
    use super::{OpenConversation, RULE_NAME, WHERE_THINGS_STAND, is_live, with_rule};
    use harness_types::TaskId;

    /// The rule is in a prompt once, whoever adds it, and names itself.
    #[test]
    fn the_rule_is_added_once() {
        let mut prompt = "system".to_owned();
        with_rule(&mut prompt);
        with_rule(&mut prompt);
        assert_eq!(prompt.matches(WHERE_THINGS_STAND).count(), 1);
        assert!(WHERE_THINGS_STAND.starts_with(&format!("# {RULE_NAME}")));
    }

    /// The texts that used to state their own order point to the rule
    /// instead, by its name, and no longer say who wins themselves.
    #[test]
    fn the_other_texts_point_to_the_rule() {
        let name = format!("\"{RULE_NAME}\"");
        let digest = super::super::harness::digest_block("entry").text;
        let earlier = super::super::task_state::earlier_block(
            &super::super::task_state::TaskState {
                objective: "x".to_owned(),
                ..Default::default()
            },
            None,
        )
        .text;
        for (what, text) in [
            ("digest", digest.as_str()),
            ("memory guidance", super::super::memory::GUIDANCE),
            ("earlier task state", earlier.as_str()),
            (
                "handoff reset",
                super::super::lifecycle::CONTINUE_FROM_HANDOFF,
            ),
        ] {
            assert!(text.contains(&name), "{what}: {text}");
            assert!(!text.contains(" wins"), "{what} ranks itself: {text}");
        }
    }

    /// The order the docs give is the order the constant gives.
    #[test]
    fn the_rule_ranks_the_records() {
        let at = |text: &str| WHERE_THINGS_STAND.find(text).expect(text);
        assert!(at("current message") < at("live task state"));
        assert!(at("live task state") < at("markdown memory"));
        assert!(at("markdown memory") < at("newest session log"));
        assert!(at("newest session log") < at(".harness/progress.md"));
        assert!(at(".harness/progress.md") < at("compaction summary"));
        assert!(WHERE_THINGS_STAND.contains("memory/decisions/"));
    }

    /// A conversation held open is live for as long as it is held, and its
    /// mark goes with the last holder.
    #[test]
    fn a_held_conversation_is_live_until_released() {
        let store = tempfile::tempdir().expect("store");
        let task = TaskId::generate();
        assert!(!is_live(&task));
        let first = OpenConversation::hold(store.path(), &task);
        let second = OpenConversation::hold(store.path(), &task);
        assert!(is_live(&task));
        let mark = store.path().join(super::OPEN_DIR).join(task.as_str());
        assert!(mark.is_file());
        drop(first);
        assert!(is_live(&task), "another holder keeps it open");
        drop(second);
        assert!(!is_live(&task));
        assert!(!mark.exists());
    }

    /// Another process's mark makes a conversation live only while that
    /// process runs.
    #[test]
    fn another_process_mark_counts_while_it_runs() {
        let store = tempfile::tempdir().expect("store");
        let own = TaskId::generate();
        // Registers the store with this process.
        let _held = OpenConversation::hold(store.path(), &own);
        let other = TaskId::generate();
        let mark = store.path().join(super::OPEN_DIR).join(other.as_str());
        // A process that is gone: pid 0 is never a user process.
        std::fs::write(&mark, "0:1").expect("mark");
        assert!(!is_live(&other));
        // A process that runs until it is killed.
        let mut running = if cfg!(windows) {
            std::process::Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(std::process::Stdio::null())
                .spawn()
        } else {
            std::process::Command::new("sleep").arg("30").spawn()
        }
        .expect("a running process");
        std::fs::write(&mark, format!("{}:1", running.id())).expect("mark");
        assert!(is_live(&other), "its owner runs");
        let _ = running.kill();
        let _ = running.wait();
        assert!(!is_live(&other), "its owner is gone");
    }
}
