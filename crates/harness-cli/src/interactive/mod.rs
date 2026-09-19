//! Interactive `ha` launch surface.
//!
//! Owner: `HA_LAUNCH` H01 — dispatch contract, terminal detection and the
//! non-terminal guard. H02 adds the resolved launch context and H03 replaces the
//! minimal boot shell with the controller/renderer pair. Nothing here starts a
//! provider, a store writer or a network call.

pub mod app;
pub mod bootstrap;
pub mod config;
pub mod detector;
pub mod headless;
pub mod paths;

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
pub fn mode_from_args(
    cwd: Option<PathBuf>,
    resume: Option<String>,
    headless: bool,
    prompt: Option<String>,
    json: bool,
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
        return Ok(LaunchMode::Interactive { cwd, resume });
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

/// Run one launch request. Usage problems return exit code 2; real failures
/// return a typed error that the binary maps to exit code 1.
pub async fn launch(mode: LaunchMode) -> Result<ExitCode, HarnessError> {
    match mode {
        LaunchMode::Headless {
            prompt,
            json,
            cwd,
            resume,
        } => headless::run(headless::HeadlessRequest {
            prompt,
            json,
            cwd,
            resume,
        }),
        LaunchMode::Interactive { cwd, resume } => {
            launch_interactive_with(&detector::SystemTerminalDetector, cwd, resume).await
        }
    }
}

/// Interactive launch with an injected detector so tests do not need a terminal.
pub async fn launch_interactive_with(
    detector: &dyn detector::TerminalDetector,
    cwd: Option<PathBuf>,
    resume: Option<String>,
) -> Result<ExitCode, HarnessError> {
    let capability = detector.capability();
    if !capability.is_interactive() {
        eprint!("{}", non_terminal_guidance(capability));
        return Ok(ExitCode::from(USAGE_EXIT_CODE));
    }
    app::run(app::AppLaunch { cwd, resume }).await
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
            mode_from_args(None, None, false, None, false).expect("bare launch is valid"),
            LaunchMode::Interactive {
                cwd: None,
                resume: None
            }
        );
        assert_eq!(
            mode_from_args(
                Some(PathBuf::from("C:/work/project")),
                Some("session_1".to_owned()),
                false,
                None,
                false
            )
            .expect("interactive launch with options is valid"),
            LaunchMode::Interactive {
                cwd: Some(PathBuf::from("C:/work/project")),
                resume: Some("session_1".to_owned())
            }
        );
    }

    #[test]
    fn h01_mode_from_args_requires_prompt_for_headless() {
        let error =
            mode_from_args(None, None, true, None, false).expect_err("headless needs a prompt");
        assert!(error.to_string().contains("--headless requires --prompt"));

        let error = mode_from_args(None, None, true, Some("   ".to_owned()), false)
            .expect_err("blank prompt is rejected");
        assert!(error.to_string().contains("must not be empty"));
    }

    #[test]
    fn h01_mode_from_args_rejects_headless_only_flags_without_headless() {
        let error = mode_from_args(None, None, false, Some("hello".to_owned()), false)
            .expect_err("prompt without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--prompt is only valid with --headless")
        );

        let error = mode_from_args(None, None, false, None, true)
            .expect_err("json without headless is rejected");
        assert!(
            error
                .to_string()
                .contains("--json is only valid with --headless")
        );
    }

    #[test]
    fn h01_headless_mode_carries_prompt_and_json_flag() {
        let mode = mode_from_args(None, None, true, Some("fix the parser".to_owned()), true)
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
