//! Session port for the interactive app.
//!
//! H03 ships the port plus two producers: a staged service that states the
//! connection is pending instead of fabricating a response, and a labelled
//! fixture that exercises the event path in tests and demos. H04 replaces the
//! staged producer with the real application service behind this same port.

use harness_types::InputId;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use super::events::{RunOutcome, SessionEvent};

/// One admitted user request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitRequest {
    pub input_id: InputId,
    pub text: String,
}

/// What the controller needs from an execution backend.
pub trait SessionPort: Send {
    /// Label shown in the header; a fixture must never look like the real thing.
    fn label(&self) -> String;
    fn submit(&mut self, request: SubmitRequest);
    fn cancel(&mut self);
}

/// Event channel shared by the port implementation and the controller.
#[derive(Debug)]
pub struct SessionChannel {
    sender: UnboundedSender<SessionEvent>,
    receiver: UnboundedReceiver<SessionEvent>,
}

impl SessionChannel {
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::unbounded_channel();
        Self { sender, receiver }
    }

    #[must_use]
    pub fn sender(&self) -> UnboundedSender<SessionEvent> {
        self.sender.clone()
    }

    /// Take everything the port produced, without blocking the render loop.
    pub fn drain(&mut self) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            events.push(event);
        }
        events
    }
}

impl Default for SessionChannel {
    fn default() -> Self {
        Self::new()
    }
}

/// Staged producer for the revisions between H03 and H04.
#[derive(Debug)]
pub struct PendingService {
    sender: UnboundedSender<SessionEvent>,
}

impl PendingService {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self { sender }
    }
}

impl SessionPort for PendingService {
    fn label(&self) -> String {
        "not connected (H04)".to_owned()
    }

    fn submit(&mut self, request: SubmitRequest) {
        let _ = self.sender.send(SessionEvent::Accepted {
            input_id: request.input_id,
        });
        let _ = self.sender.send(SessionEvent::RecoverableError {
            message: "connection pending: no application service is wired in this revision (HA_LAUNCH H04); nothing was sent and no response was fabricated".to_owned(),
        });
    }

    fn cancel(&mut self) {
        let _ = self.sender.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Canceled,
        });
    }
}

/// Deterministic labelled fixture used by tests and explicit demos.
#[derive(Debug)]
pub struct FixtureService {
    sender: UnboundedSender<SessionEvent>,
}

impl FixtureService {
    #[must_use]
    pub fn new(sender: UnboundedSender<SessionEvent>) -> Self {
        Self { sender }
    }
}

impl SessionPort for FixtureService {
    fn label(&self) -> String {
        "fixture (no model was called)".to_owned()
    }

    fn submit(&mut self, request: SubmitRequest) {
        let send = |event| {
            let _ = self.sender.send(event);
        };
        send(SessionEvent::Accepted {
            input_id: request.input_id,
        });
        // Echoing the admitted text makes the transcript prove exactly which
        // buffer was submitted, which is what the editor tests assert on.
        send(SessionEvent::TextDelta {
            text: format!("fixture answer for: {}", request.text),
        });
        send(SessionEvent::TextDelta {
            text: " (no model was called)".to_owned(),
        });
        send(SessionEvent::ToolStarted {
            name: "search_text".to_owned(),
            summary: "pattern=parser".to_owned(),
        });
        send(SessionEvent::ToolSettled {
            name: "search_text".to_owned(),
            ok: false,
        });
        send(SessionEvent::ToolStarted {
            name: "apply_patch".to_owned(),
            summary: "path=src/parser.rs".to_owned(),
        });
        send(SessionEvent::ToolSettled {
            name: "apply_patch".to_owned(),
            ok: true,
        });
        // A request that asks for a failure exercises the failure rendering
        // deterministically, without pretending a model produced it.
        let outcome = if request.text.contains("fail") {
            RunOutcome::Failed("fixture failure requested by the prompt".to_owned())
        } else {
            RunOutcome::Done
        };
        send(SessionEvent::RunTerminal { outcome });
    }

    fn cancel(&mut self) {
        let _ = self.sender.send(SessionEvent::RunTerminal {
            outcome: RunOutcome::Canceled,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{FixtureService, PendingService, SessionChannel, SessionPort, SubmitRequest};
    use crate::interactive::events::{RunOutcome, SessionEvent};
    use harness_types::InputId;

    fn request() -> SubmitRequest {
        SubmitRequest {
            input_id: InputId::generate(),
            text: "fix the parser".to_owned(),
        }
    }

    #[test]
    fn h03_pending_service_accepts_then_reports_that_nothing_ran() {
        let mut channel = SessionChannel::new();
        let mut service = PendingService::new(channel.sender());
        assert!(service.label().contains("not connected"));
        service.submit(request());

        let events = channel.drain();
        assert!(matches!(events[0], SessionEvent::Accepted { .. }));
        let SessionEvent::RecoverableError { message } = &events[1] else {
            panic!("expected a recoverable error, got {events:?}");
        };
        assert!(message.contains("connection pending"), "{message}");
        assert!(matches!(events[1], SessionEvent::RecoverableError { .. }));
    }

    #[test]
    fn h03_fixture_service_is_labelled_and_streams_a_full_run() {
        let mut channel = SessionChannel::new();
        let mut service = FixtureService::new(channel.sender());
        assert!(service.label().contains("fixture"));
        service.submit(request());

        let events = channel.drain();
        assert!(matches!(events[0], SessionEvent::Accepted { .. }));
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::TextDelta { .. }))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, SessionEvent::ToolStarted { .. }))
                .count(),
            2
        );
        let SessionEvent::RunTerminal { outcome } = events.last().expect("terminal event") else {
            panic!("the fixture must end the run: {events:?}");
        };
        assert_eq!(outcome, &RunOutcome::Done);
        assert!(channel.drain().is_empty(), "draining empties the channel");
    }

    #[test]
    fn h03_cancel_reports_a_canceled_run() {
        let mut channel = SessionChannel::new();
        let mut service = FixtureService::new(channel.sender());
        service.cancel();
        assert_eq!(
            channel.drain(),
            vec![SessionEvent::RunTerminal {
                outcome: RunOutcome::Canceled
            }]
        );
    }
}
