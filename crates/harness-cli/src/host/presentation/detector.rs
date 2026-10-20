//! Terminal capability detection.
//!
//! The detector is a trait so unit tests and fixture hosts can decide the
//! capability instead of depending on the terminal that happens to run them.
//! Only the binary uses the real detector; the plan forbids a fake detector as
//! integration proof.

/// What the process can assume about its own standard streams.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapability {
    pub stdin_is_terminal: bool,
    pub stdout_is_terminal: bool,
}

impl TerminalCapability {
    /// The interactive app needs both streams, because it owns the prompt and
    /// writes the transcript. One redirected stream is enough to refuse.
    #[must_use]
    pub const fn is_interactive(self) -> bool {
        self.stdin_is_terminal && self.stdout_is_terminal
    }

    #[cfg(test)]
    #[must_use]
    pub const fn interactive() -> Self {
        Self {
            stdin_is_terminal: true,
            stdout_is_terminal: true,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub const fn piped() -> Self {
        Self {
            stdin_is_terminal: false,
            stdout_is_terminal: false,
        }
    }
}

/// Injected terminal capability source.
pub trait TerminalDetector: Send + Sync {
    fn capability(&self) -> TerminalCapability;
}

/// Real detector used by the shipped binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemTerminalDetector;

impl TerminalDetector for SystemTerminalDetector {
    fn capability(&self) -> TerminalCapability {
        use std::io::IsTerminal;

        TerminalCapability {
            stdin_is_terminal: std::io::stdin().is_terminal(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
        }
    }
}

/// Deterministic detector for unit tests; integration proof must use a real
/// terminal instead of this fixture.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub struct FixedTerminalDetector {
    capability: TerminalCapability,
}

#[cfg(test)]
impl FixedTerminalDetector {
    #[must_use]
    pub const fn new(capability: TerminalCapability) -> Self {
        Self { capability }
    }
}

#[cfg(test)]
impl TerminalDetector for FixedTerminalDetector {
    fn capability(&self) -> TerminalCapability {
        self.capability
    }
}

#[cfg(test)]
mod tests {
    use super::{FixedTerminalDetector, TerminalCapability, TerminalDetector};

    #[test]
    fn h01_capability_requires_both_streams() {
        assert!(TerminalCapability::interactive().is_interactive());
        assert!(!TerminalCapability::piped().is_interactive());
        assert!(
            !TerminalCapability {
                stdin_is_terminal: true,
                stdout_is_terminal: false
            }
            .is_interactive()
        );
        assert!(
            !TerminalCapability {
                stdin_is_terminal: false,
                stdout_is_terminal: true
            }
            .is_interactive()
        );
    }

    #[test]
    fn h01_fixed_detector_reports_the_injected_capability() {
        assert_eq!(
            FixedTerminalDetector::new(TerminalCapability::piped()).capability(),
            TerminalCapability::piped()
        );
        assert_eq!(
            FixedTerminalDetector::new(TerminalCapability::interactive()).capability(),
            TerminalCapability::interactive()
        );
    }
}
