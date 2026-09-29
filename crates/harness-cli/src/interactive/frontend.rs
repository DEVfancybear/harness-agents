//! What the TUI drives: the app's controller in this process, or a background
//! agent's controller over its socket (`ha attach`).
//!
//! The TUI never reaches into the controller beyond these calls, and what they
//! carry - keys in, [`Effect`]s and a [`UiState`] out - is plain data. That is
//! the seam prime-agent's daemon puts between a client and its session worker:
//! the worker owns the conversation, its turns and its schedules, and a client
//! only draws them, so closing the client leaves the work running.

use super::controller::{Effect, InteractiveController};
use super::events::{Key, UiState};

pub trait Frontend {
    /// Continue a persisted session before the first frame (`--resume`).
    ///
    /// # Errors
    /// The session cannot be selected.
    fn resume_source(&mut self, session_id: &str) -> Result<(), String>;
    fn set_columns(&mut self, columns: u16);
    /// The header drawn once, above the conversation.
    fn boot_lines(&mut self) -> Vec<String>;
    fn ui_state(&self) -> UiState;
    fn handle_key(&mut self, key: Key) -> Vec<Effect>;
    /// What the session produced since the last call.
    fn pump_events(&mut self) -> Vec<Effect>;
    /// The spinner and the clocks.
    fn tick(&mut self) -> Vec<Effect>;
}

impl Frontend for InteractiveController {
    fn resume_source(&mut self, session_id: &str) -> Result<(), String> {
        Self::resume_source(self, session_id)
    }

    fn set_columns(&mut self, columns: u16) {
        Self::set_columns(self, columns);
    }

    fn boot_lines(&mut self) -> Vec<String> {
        Self::boot_lines(self)
    }

    fn ui_state(&self) -> UiState {
        Self::ui_state(self)
    }

    fn handle_key(&mut self, key: Key) -> Vec<Effect> {
        Self::handle_key(self, key)
    }

    fn pump_events(&mut self) -> Vec<Effect> {
        Self::pump_events(self)
    }

    fn tick(&mut self) -> Vec<Effect> {
        Self::tick(self)
    }
}
