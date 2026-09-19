//! Interactive event vocabulary for `HA_LAUNCH` H03.
//!
//! Terminal input is normalized into the Key enum so the editor, the controller
//! and their tests never depend on a terminal library type. Backend work is
//! normalized into `SessionEvent`; H04 replaces the staged producer of those
//! events with the application service and this vocabulary stays.

use harness_types::InputId;

/// One normalized terminal input event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    Enter,
    /// Ctrl-C: cancel an active run, or clear an idle prompt.
    Interrupt,
    /// Ctrl-D on an empty prompt: leave the app.
    EndOfInput,
    /// Bracketed paste; embedded newlines must never become separate commands.
    Paste(String),
    Resize {
        columns: u16,
        rows: u16,
    },
    Unknown,
}

/// Phase of the interactive app, as described in the `HA_LAUNCH` plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppPhase {
    Booting,
    Ready,
    SetupRequired,
    Running,
    Canceling,
    Closed,
}

impl AppPhase {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Booting => "booting",
            Self::Ready => "ready",
            Self::SetupRequired => "setup_required",
            Self::Running => "running",
            Self::Canceling => "canceling",
            Self::Closed => "closed",
        }
    }

    /// A run is active, so a second input must not be admitted.
    #[must_use]
    pub const fn has_active_run(self) -> bool {
        matches!(self, Self::Running | Self::Canceling)
    }
}

/// How one user turn ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RunOutcome {
    Done,
    Failed(String),
    Canceled,
}

impl RunOutcome {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Done => "done".to_owned(),
            Self::Failed(reason) => format!("failed: {reason}"),
            Self::Canceled => "canceled".to_owned(),
        }
    }
}

/// Events the controller consumes from the session port.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    Accepted { input_id: InputId },
    TextDelta { text: String },
    ToolStarted { name: String, summary: String },
    ToolSettled { name: String, ok: bool },
    RunTerminal { outcome: RunOutcome },
    RecoverableError { message: String },
}

#[cfg(test)]
mod tests {
    use super::{AppPhase, RunOutcome, SessionEvent};
    use harness_types::InputId;

    #[test]
    fn h03_phase_labels_and_active_run_are_explicit() {
        assert_eq!(AppPhase::SetupRequired.label(), "setup_required");
        assert!(AppPhase::Running.has_active_run());
        assert!(AppPhase::Canceling.has_active_run());
        assert!(!AppPhase::Ready.has_active_run());
        assert!(!AppPhase::Closed.has_active_run());
    }

    #[test]
    fn h03_terminal_outcomes_are_labelled_for_the_transcript() {
        assert_eq!(RunOutcome::Canceled.label(), "canceled");
        assert_eq!(RunOutcome::Done.label(), "done");
        assert!(
            RunOutcome::Failed("boom".to_owned())
                .label()
                .contains("boom")
        );

        let input_id = InputId::generate();
        let events = [
            SessionEvent::Accepted { input_id },
            SessionEvent::TextDelta {
                text: "hi".to_owned(),
            },
            SessionEvent::ToolStarted {
                name: "read_file".to_owned(),
                summary: "path=a.rs".to_owned(),
            },
            SessionEvent::ToolSettled {
                name: "read_file".to_owned(),
                ok: true,
            },
            SessionEvent::RunTerminal {
                outcome: RunOutcome::Done,
            },
            SessionEvent::RecoverableError {
                message: "connection pending".to_owned(),
            },
        ];
        assert_eq!(events.len(), 6, "every vocabulary item stays representable");
    }
}
