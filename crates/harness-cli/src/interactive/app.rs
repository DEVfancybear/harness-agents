//! Minimal interactive boot shell for `HA_LAUNCH` H01/H02.
//!
//! H01 must prove that a bare `ha` dispatch reaches a live interactive process
//! that reads input and only exits when the user exits. H02 replaces the
//! placeholder context with the resolved launch context. The real terminal app —
//! raw mode, line editor, streaming renderer — is H03, and the agent service is
//! H04, so text input is still not sent anywhere and the header says so.

use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError};

use super::bootstrap::{self, LaunchContext, LaunchRequest};
use super::paths::{HostPlatform, LaunchEnvironment};

/// Validated interactive launch request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppLaunch {
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
}

/// Input events delivered by the prompt reader thread.
#[derive(Clone, Debug, Eq, PartialEq)]
enum InputEvent {
    Line(String),
    EndOfInput,
    ReadFailed(String),
}

/// Run the interactive shell until the user exits.
pub async fn run(launch: AppLaunch) -> Result<ExitCode, HarnessError> {
    let context = resolve_context(&launch)?;
    let mut output = std::io::stdout();
    render_boot(&mut output, &context, launch.resume.as_deref())?;

    let (sender, mut receiver) = tokio::sync::mpsc::channel::<InputEvent>(16);
    std::thread::spawn(move || {
        use std::io::BufRead;

        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match handle.read_line(&mut line) {
                Ok(0) => {
                    let _ = sender.blocking_send(InputEvent::EndOfInput);
                    return;
                }
                Ok(_) => {
                    let event = InputEvent::Line(line.trim_end_matches(['\r', '\n']).to_owned());
                    if sender.blocking_send(event).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = sender.blocking_send(InputEvent::ReadFailed(error.to_string()));
                    return;
                }
            }
        }
    });

    while let Some(event) = receiver.recv().await {
        match event {
            InputEvent::Line(line) => {
                match LineAction::parse(&line) {
                    LineAction::Exit => {
                        writeln!(output, "bye").map_err(|error| write_failed(&error))?;
                        return Ok(ExitCode::SUCCESS);
                    }
                    LineAction::Help => {
                        write!(output, "{}", help_text()).map_err(|error| write_failed(&error))?;
                    }
                    LineAction::Status => render_status(&mut output, &context)?,
                    LineAction::Empty => {}
                    LineAction::Prompt => {
                        writeln!(
                            output,
                            "connection pending: no application service is wired in this revision (HA_LAUNCH H04), so nothing was sent."
                        )
                        .map_err(|error| write_failed(&error))?;
                    }
                    LineAction::Unknown(command) => {
                        writeln!(
                            output,
                            "unknown command {command}; /help lists what this revision supports."
                        )
                        .map_err(|error| write_failed(&error))?;
                    }
                }
                write!(output, "> ").map_err(|error| write_failed(&error))?;
                output.flush().map_err(|error| write_failed(&error))?;
            }
            InputEvent::EndOfInput => {
                writeln!(output, "bye").map_err(|error| write_failed(&error))?;
                return Ok(ExitCode::SUCCESS);
            }
            InputEvent::ReadFailed(message) => {
                return Err(HarnessError::new(
                    ErrorCode::ConfigReadError,
                    format!("interactive input could not be read: {message}"),
                ));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Resolve the launch context from the real environment.
///
/// The platform and environment are injected into the resolver, so the same code
/// path is unit tested with a fixture home instead of the developer profile.
fn resolve_context(launch: &AppLaunch) -> Result<LaunchContext, HarnessError> {
    let caller_dir = std::env::current_dir().map_err(|error| {
        HarnessError::new(
            ErrorCode::StorageOpenFailed,
            format!("the current working directory could not be resolved: {error}"),
        )
    })?;
    bootstrap::resolve(LaunchRequest {
        cwd: launch.cwd.clone(),
        caller_dir,
        platform: HostPlatform::current(),
        environment: LaunchEnvironment::capture(),
        explicit_data_dir: None,
    })
}

fn render_boot(
    output: &mut impl Write,
    context: &LaunchContext,
    resume: Option<&str>,
) -> Result<(), HarnessError> {
    let mut text = String::from("\n");
    for line in context.header_lines() {
        text.push_str(&line);
        text.push('\n');
    }
    let session = resume.map_or_else(
        || "new".to_owned(),
        |session| format!("resume {session} (pending H05)"),
    );
    let _ = writeln!(text, "Session: {session}    Mode: trusted host");
    if let Some(hint) = context.setup_hint() {
        text.push_str(&hint);
        text.push('\n');
    }
    text.push_str("\nNhập yêu cầu. /help trợ giúp · /status chẩn đoán · /exit thoát\n> ");
    output
        .write_all(text.as_bytes())
        .and_then(|()| output.flush())
        .map_err(|error| write_failed(&error))
}

fn render_status(output: &mut impl Write, context: &LaunchContext) -> Result<(), HarnessError> {
    let mut text = String::new();
    for line in context.header_lines() {
        text.push_str(&line);
        text.push('\n');
    }
    let _ = writeln!(text, "Identity: {}", context.project.digest.as_str());
    let _ = writeln!(text, "Caller:   {}", context.caller_dir.display());
    output
        .write_all(text.as_bytes())
        .map_err(|error| write_failed(&error))
}

/// What a submitted line means before H03 replaces this parser.
#[derive(Clone, Debug, Eq, PartialEq)]
enum LineAction {
    Empty,
    Exit,
    Help,
    Status,
    Prompt,
    Unknown(String),
}

impl LineAction {
    fn parse(line: &str) -> Self {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Self::Empty;
        }
        let Some(command) = trimmed.strip_prefix('/') else {
            return Self::Prompt;
        };
        let mut parts = command.split_whitespace();
        let name = parts.next().unwrap_or_default();
        let arguments = parts.next();
        match (name, arguments) {
            ("exit" | "quit", None) => Self::Exit,
            ("help", None) => Self::Help,
            ("status", None) => Self::Status,
            _ => Self::Unknown(format!("/{}", command.trim())),
        }
    }
}

fn help_text() -> &'static str {
    "commands: /help, /status, /exit. The full set (/new, /model, /config, /resume) arrives with HA_LAUNCH H03/H05.\n"
}

fn write_failed(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageWriteFailed,
        format!("interactive output could not be written: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{LineAction, render_boot};
    use crate::interactive::bootstrap::{self, LaunchRequest};
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};

    #[test]
    fn h01_line_action_parses_slash_commands_without_treating_text_as_commands() {
        assert_eq!(LineAction::parse("   "), LineAction::Empty);
        assert_eq!(LineAction::parse("/exit"), LineAction::Exit);
        assert_eq!(LineAction::parse("  /quit  "), LineAction::Exit);
        assert_eq!(LineAction::parse("/help"), LineAction::Help);
        assert_eq!(LineAction::parse("/status"), LineAction::Status);
        assert_eq!(
            LineAction::parse("fix the parser"),
            LineAction::Prompt,
            "free text is never a command"
        );
        assert_eq!(
            LineAction::parse("rm -rf /"),
            LineAction::Prompt,
            "input is not a shell command"
        );
        assert!(matches!(
            LineAction::parse("/model gpt"),
            LineAction::Unknown(_)
        ));
    }

    #[test]
    fn h02_boot_render_uses_the_resolved_context_and_keeps_the_prompt_alive() {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project with spaces");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project.clone(),
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([(
                "HA_HOME",
                home.to_string_lossy().into_owned(),
            )]),
            explicit_data_dir: None,
        })
        .expect("context resolves");

        let mut buffer = Vec::new();
        render_boot(&mut buffer, &context, Some("session_01")).expect("boot render");
        let text = String::from_utf8(buffer).expect("boot output is UTF-8");

        assert!(text.contains("Harness Agents"), "{text}");
        assert!(text.contains(&project.display().to_string()), "{text}");
        assert!(text.contains("setup required"), "{text}");
        assert!(text.contains("resume session_01 (pending H05)"), "{text}");
        assert!(text.contains("HA_HOME"), "{text}");
        assert!(text.ends_with("> "), "the prompt stays open: {text:?}");
    }
}
