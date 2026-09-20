//! Interactive `ha` launch surface.
//!
//! Owner: `HA_LAUNCH` H01 — dispatch contract, terminal detection and the
//! non-terminal guard. H02 adds the resolved launch context and H03 replaces the
//! minimal boot shell with the controller/renderer pair. Nothing here starts a
//! provider, a store writer or a network call.

pub mod app;
pub mod bootstrap;
pub mod config;
pub mod controller;
pub mod credentials;
pub mod detector;
pub mod events;
pub mod headless;
pub mod input;
pub mod memory;
pub mod paths;
pub mod project;
pub mod service;
pub mod terminal;
pub mod tui;
pub mod view;

use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::HarnessError;

/// Exit code reserved for usage violations and the non-terminal guard; it is the
/// same code clap uses for parser errors.
pub const USAGE_EXIT_CODE: u8 = 2;

/// A launch request that the parser accepted but the launch contract rejects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageError {
    message: String,
}

impl UsageError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "ha: {}", self.message)
    }
}

/// The launch shape selected by the dispatch layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LaunchMode {
    /// Open the interactive app on a real terminal.
    Interactive {
        cwd: Option<PathBuf>,
        resume: Option<String>,
        /// Explicit opt-in to the labelled fixture backend; never a default and
        /// never a silent production fallback.
        fixture: bool,
        /// Force the plain renderer instead of the TUI.
        ///
        /// Set by `ha chat --plain` or by `HA_UI=plain`; the TUI is the default on
        /// a console that can hold it.
        plain: bool,
    },
    /// Run exactly one turn without a terminal.
    Headless {
        prompt: String,
        json: bool,
        cwd: Option<PathBuf>,
        resume: Option<String>,
    },
}

/// Validate parser output into a launch mode.
///
/// Clap already enforces the flag conflicts, so this is the typed backstop that
/// keeps the contract testable without spawning a process, and that keeps a
/// missing prompt from ever being treated as free text.
#[allow(clippy::fn_params_excessive_bools, reason = "one flag per launch mode")]
pub fn mode_from_args(
    cwd: Option<PathBuf>,
    resume: Option<String>,
    fixture: bool,
    headless: bool,
    prompt: Option<String>,
    json: bool,
    plain: bool,
) -> Result<LaunchMode, UsageError> {
    if !headless {
        if prompt.is_some() {
            return Err(UsageError::new(
                "--prompt is only valid with --headless; run ha chat --headless --prompt <text>",
            ));
        }
        if json {
            return Err(UsageError::new(
                "--json is only valid with --headless; run ha chat --headless --prompt <text> --json",
            ));
        }
        return Ok(LaunchMode::Interactive {
            cwd,
            resume,
            fixture,
            // Clap already rejects `--plain --headless`; the typed backstop keeps
            // a headless run from ever being routed through the TUI.
            plain,
        });
    }
    if fixture {
        return Err(UsageError::new(
            "--fixture is only valid for the interactive app; a headless turn must report the real backend state",
        ));
    }
    if plain {
        return Err(UsageError::new(
            "--plain is only valid for the interactive app; a headless turn never draws a viewport",
        ));
    }
    let Some(prompt) = prompt else {
        return Err(UsageError::new(
            "--headless requires --prompt <text>; the prompt is never read from stdin",
        ));
    };
    if prompt.trim().is_empty() {
        return Err(UsageError::new("--prompt must not be empty"));
    }
    Ok(LaunchMode::Headless {
        prompt,
        json,
        cwd,
        resume,
    })
}

/// Environment variable that selects the renderer explicitly.
pub const UI_VARIABLE: &str = "HA_UI";

/// Whether the environment asks for the plain renderer.
///
/// Only the exact value `plain` counts: an unknown `HA_UI` value is not a silent
/// opt-in to anything, and the TUI stays the default.
#[must_use]
pub fn plain_requested(ui: Option<&str>) -> bool {
    matches!(ui, Some(value) if value.eq_ignore_ascii_case("plain"))
}

/// Read [`UI_VARIABLE`] from the process environment.
#[must_use]
pub fn plain_requested_from_environment() -> bool {
    plain_requested(std::env::var(UI_VARIABLE).ok().as_deref())
}

/// Run one launch request. Usage problems return exit code 2; real failures
/// return a typed error that the binary maps to exit code 1.
pub async fn launch(mode: LaunchMode) -> Result<ExitCode, HarnessError> {
    match mode {
        LaunchMode::Headless {
            prompt,
            json,
            cwd,
            resume,
        } => {
            headless::run(headless::HeadlessRequest {
                prompt,
                json,
                cwd,
                resume,
            })
            .await
        }
        LaunchMode::Interactive {
            cwd,
            resume,
            fixture,
            plain,
        } => {
            launch_interactive_with(
                &detector::SystemTerminalDetector,
                cwd,
                resume,
                fixture,
                plain,
            )
            .await
        }
    }
}

/// Interactive launch with an injected detector so tests do not need a terminal.
pub async fn launch_interactive_with(
    detector: &dyn detector::TerminalDetector,
    cwd: Option<PathBuf>,
    resume: Option<String>,
    fixture: bool,
    plain: bool,
) -> Result<ExitCode, HarnessError> {
    let capability = detector.capability();
    if !capability.is_interactive() {
        eprint!("{}", non_terminal_guidance(capability));
        return Ok(ExitCode::from(USAGE_EXIT_CODE));
    }
    app::run(app::AppLaunch {
        cwd,
        resume,
        fixture,
        plain,
    })
    .await
}

/// Guidance printed to stderr when the interactive app cannot own a terminal.
#[must_use]
pub fn non_terminal_guidance(capability: detector::TerminalCapability) -> String {
    format!(
        "ha: no interactive terminal (stdin tty: {}, stdout tty: {}).\n\
         The interactive app needs a terminal it can own, so it was not started and stdin was not read.\n\
         Run one turn explicitly instead:\n  ha chat --headless --prompt \"<your request>\" [--json]\n",
        capability.stdin_is_terminal, capability.stdout_is_terminal
    )
}

#[cfg(test)]
mod tests {
    use super::{LaunchMode, USAGE_EXIT_CODE, mode_from_args, non_terminal_guidance};
    use crate::interactive::detector::TerminalCapability;
    use std::path::PathBuf;

    #[test]
    fn h01_mode_from_args_accepts_bare_and_optioned_interactive_launch() {
        assert_eq!(
            mode_from_args(None, None, false, false, None, false, false)
                .expect("bare launch is valid"),
            LaunchMode::Interactive {
                cwd: None,
                resume: None,
                fixture: false,
                plain: false,
            }
        );
        assert_eq!(
            mode_from_args(
                Some(PathBuf::from("C:/work/project")),
                Some("session_1".to_owned()),
                true,
                false,
                None,
                false,
                true
            )
            .expect("interactive launch with options is valid"),
            LaunchMode::Interactive {
                cwd: Some(PathBuf::from("C:/work/project")),
                resume: Some("session_1".to_owned()),
                fixture: true,
                plain: true,
            }
        );
    }

    #[test]
    fn t07_plain_is_rejected_for_a_headless_turn() {
        let error = mode_from_args(None, None, false, true, Some("hi".to_owned()), false, true)
            .expect_err("a headless turn never draws a viewport");
        assert!(error.to_string().contains("--plain is only valid"));
    }

    #[test]
    fn t07_plain_requested_only_honours_the_exact_value() {
        assert!(super::plain_requested(Some("plain")));
        assert!(super::plain_requested(Some("PLAIN")));
        assert!(!super::plain_requested(Some("tui")));
        assert!(!super::plain_requested(Some("")));
        assert!(!super::plain_requested(None));
    }

    #[test]
    fn h01_mode_from_args_requires_prompt_for_headless() {
        let error = mode_from_args(None, None, false, true, None, false, false)
            .expect_err("headless needs a prompt");
        assert!(error.to_string().contains("--headless requires --prompt"));

        let error = mode_from_args(
            None,
            None,
            false,
            true,
            Some("   ".to_owned()),
            false,
            false,
        )
        .expect_err("blank prompt is rejected");
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn h01_mode_from_args_rejects_headless_only_flags_without_headless() {
        let error = mode_from_args(
            None,
            None,
            false,
            false,
            Some("hello".to_owned()),
            false,
            false,
        )
        .expect_err("prompt without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--prompt is only valid with --headless")
        );

        let error = mode_from_args(None, None, false, false, None, true, false)
            .expect_err("json without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--json is only valid with --headless")
        );
    }

    #[test]
    fn h03_fixture_is_rejected_for_a_headless_turn() {
        let error = mode_from_args(
            None,
            None,
            true,
            true,
            Some("fix the parser".to_owned()),
            false,
            false,
        )
        .expect_err("a headless turn must not silently use the fixture");
        assert!(error.to_string().contains("--fixture is only valid"));
    }

    #[test]
    fn h01_headless_mode_carries_prompt_and_json_flag() {
        let mode = mode_from_args(
            None,
            None,
            false,
            true,
            Some("fix the parser".to_owned()),
            true,
            false,
        )
        .expect("headless launch is valid");
        assert_eq!(
            mode,
            LaunchMode::Headless {
                prompt: "fix the parser".to_owned(),
                json: true,
                cwd: None,
                resume: None
            }
        );
    }

    #[test]
    fn h01_non_terminal_guidance_points_at_the_explicit_headless_command() {
        let guidance = non_terminal_guidance(TerminalCapability::piped());
        assert!(guidance.contains("stdin tty: false"));
        assert!(guidance.contains("stdout tty: false"));
        assert!(guidance.contains("ha chat --headless --prompt"));
        assert_eq!(USAGE_EXIT_CODE, 2);
    }
}
