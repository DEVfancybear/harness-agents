//! Non-interactive launch policy: what a schedule may do with nobody attached.
//!
//! A schedule captures the authority of the user who created it and never more
//! (`LaunchGrants`, and `auto_approve_tools` is always false). A scheduled launch
//! has no human at the keyboard, so the policy here is about the one case where
//! that matters: work that would change a workspace.
//!
//! - A **read-only** launch runs: it was authorised when the schedule was
//!   created, and there is nothing to ask.
//! - A launch that would **edit a workspace** is claimed (so it can never launch
//!   twice) and then held `waiting` for an explicit decision, with a window after
//!   which it expires. Nothing is auto-approved, and nothing is silently skipped
//!   either: the wait is durable and visible in the daemon's status.
//!
//! The decision itself is a row, not a state of mind: [`resolve_waiting`] reads
//! what a human actually answered and what the clock says, and returns the only
//! three things that can follow.

use harness_store_sqlite::StoredApproval;

use super::LaunchGrants;

/// How long a waiting occurrence may wait before its window closes.
pub const APPROVAL_WINDOW_MS: i64 = 60 * 60 * 1000;

/// What a waiting occurrence resolves to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitingResolution {
    /// Nobody has decided yet and the window is still open: keep waiting.
    Wait,
    /// A human approved it: launch it, once.
    Launch,
    /// A human denied it: record the refusal instead of running anyway.
    Skip,
    /// Nobody answered inside the window.
    Expire,
}

/// Whether a launch needs a decision before it may run.
///
/// The rule is the narrow one: a launch that changes a workspace is the kind of
/// action an interactive run would ask about, so a scheduled one asks too. A
/// read-only launch does not, which is what keeps a reporting schedule useful
/// without a human watching it.
#[must_use]
pub fn requires_approval(grants: &LaunchGrants) -> bool {
    grants.edit_workspace
}

/// What a decision and a clock say about one waiting occurrence.
///
/// `now_unix_ms` is passed in rather than read, so the same function answers the
/// question in a test with a fixed clock and in the daemon with a real one.
#[must_use]
pub fn resolve_waiting(approval: &StoredApproval, now_unix_ms: i64) -> WaitingResolution {
    match approval.state.as_str() {
        "approved" => WaitingResolution::Launch,
        "denied" => WaitingResolution::Skip,
        // Open: the window decides. An open question is never a yes, however
        // long it has been open. `expired`, and any state this build does not
        // know, end the same way: nothing was approved, so nothing launches.
        "open" if now_unix_ms < approval.expires_at_unix_ms => WaitingResolution::Wait,
        _ => WaitingResolution::Expire,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(state: &str, expires_at_unix_ms: i64) -> StoredApproval {
        StoredApproval {
            approval_id: "approval_1".to_owned(),
            occurrence_key: "schedule#1#2".to_owned(),
            schedule_id: "nightly".to_owned(),
            revision: 1,
            state: state.to_owned(),
            prompt: "approve the nightly edit".to_owned(),
            requested_at_unix_ms: 1_000,
            expires_at_unix_ms,
            decided_at_unix_ms: None,
            decided_by: None,
            reason: None,
        }
    }

    #[test]
    fn only_an_approval_launches() {
        assert_eq!(
            resolve_waiting(&approval("approved", 10_000), 5_000),
            WaitingResolution::Launch
        );
        assert_eq!(
            resolve_waiting(&approval("denied", 10_000), 5_000),
            WaitingResolution::Skip
        );
    }

    #[test]
    fn an_open_window_never_launches() {
        // Not even at the last instant: an unanswered question is not a yes.
        assert_eq!(
            resolve_waiting(&approval("open", 10_000), 9_999),
            WaitingResolution::Wait
        );
        assert_eq!(
            resolve_waiting(&approval("open", 10_000), 10_000),
            WaitingResolution::Expire
        );
        assert_eq!(
            resolve_waiting(&approval("open", 10_000), 20_000),
            WaitingResolution::Expire
        );
        assert_eq!(
            resolve_waiting(&approval("expired", 10_000), 1_000),
            WaitingResolution::Expire
        );
        assert_eq!(
            resolve_waiting(&approval("something_else", 10_000), 1_000),
            WaitingResolution::Expire,
            "an unknown state is undecided, and undecided never launches"
        );
    }

    #[test]
    fn only_a_workspace_edit_needs_a_human() {
        let mut grants = LaunchGrants {
            principal_id: "user".to_owned(),
            project_id: None,
            task_id: None,
            edit_workspace: false,
            budget_tokens: 1_000,
            auto_approve_tools: false,
        };
        assert!(!requires_approval(&grants));
        grants.edit_workspace = true;
        assert!(requires_approval(&grants));
        assert!(
            !grants.auto_approve_tools,
            "a schedule never carries auto-approval, whatever it asks to do"
        );
    }
}
