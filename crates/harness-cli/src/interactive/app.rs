//! Minimal interactive boot shell for `HA_LAUNCH` H01.
//!
//! H01 must prove that a bare `ha` dispatch reaches a live interactive process
//! that reads input and only exits when the user exits. The real terminal app —
//! raw mode, line editor, streaming renderer — is H03, and the agent service is
//! H04. Until then this shell states its state honestly instead of pretending to
//! be connected: text input is not sent anywhere, and the header says so.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use harness_types::{ErrorCode, HarnessError};

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
    let context = BootContext::resolve(&launch)?;
    let mut output = std::io::stdout();
    write!(output, "{}", context.header()).map_err(|error| write_failed(&error))?;
    output.flush().map_err(|error| write_failed(&error))?;

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
                let action = LineAction::parse(&line);
                match action {
                    LineAction::Exit => {
                        writeln!(output, "bye").map_err(|error| write_failed(&error))?;
                        return Ok(ExitCode::SUCCESS);
                    }
                    LineAction::Help => {
                        write!(output, "{}", help_text()).map_err(|error| write_failed(&error))?;
                    }
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
                    ErrorCode::StorageOpenFailed,
                    format!("interactive input could not be read: {message}"),
                ));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// What a submitted line means before H03 replaces this parser.
#[derive(Clone, Debug, Eq, PartialEq)]
enum LineAction {
    Empty,
    Exit,
    Help,
    Prompt,
    Unknown(String),
}

impl LineAction {
    fn parse(line: &str) -> Self {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Self::Empty;
        }
        if let Some(command) = trimmed.strip_prefix('/') {
            let mut parts = command.split_whitespace();
            let name = parts.next().unwrap_or_default();
            let arguments = parts.next();
            return match (name, arguments) {
                ("exit" | "quit", None) => Self::Exit,
                ("help", None) => Self::Help,
                _ => Self::Unknown(format!("/{}", command.trim())),
            };
        }
        Self::Prompt
    }
}

/// Everything the boot shell needs to render its header.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BootContext {
    version: String,
    project_dir: PathBuf,
    provider: String,
    session: String,
    mode: String,
}

impl BootContext {
    /// H01 resolves the caller directory only; H02 replaces this with the
    /// resolved config/data/project bootstrap.
    fn resolve(launch: &AppLaunch) -> Result<Self, HarnessError> {
        let project_dir = match &launch.cwd {
            Some(path) => path.clone(),
            None => std::env::current_dir().map_err(|error| {
                HarnessError::new(
                    ErrorCode::StorageOpenFailed,
                    format!("the current working directory could not be resolved: {error}"),
                )
            })?,
        };
        let session = launch.resume.as_ref().map_or_else(
            || "new".to_owned(),
            |session| format!("resume {session} (pending H05)"),
        );
        Ok(Self {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            project_dir,
            provider: "setup pending (H04)".to_owned(),
            session,
            mode: "trusted host".to_owned(),
        })
    }

    fn header(&self) -> String {
        format!(
            "\nHarness Agents {version}\nProject: {project}    Provider: {provider}\nSession: {session}    Mode: {mode}\n\nNhap yeu cau. /help tro giup - /exit thoat\n> ",
            version = self.version,
            project = self.project_dir.display(),
            provider = self.provider,
            session = self.session,
            mode = self.mode,
        )
    }
}

fn help_text() -> &'static str {
    "commands: /help, /exit. The full set (/new, /status, /model, /config, /resume) arrives with HA_LAUNCH H03/H05.\n"
}

fn write_failed(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageWriteFailed,
        format!("interactive output could not be written: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{BootContext, LineAction};

    #[test]
    fn h01_line_action_parses_slash_commands_without_treating_text_as_commands() {
        assert_eq!(LineAction::parse("   "), LineAction::Empty);
        assert_eq!(LineAction::parse("/exit"), LineAction::Exit);
        assert_eq!(LineAction::parse("  /quit  "), LineAction::Exit);
        assert_eq!(LineAction::parse("/help"), LineAction::Help);
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
    fn h01_boot_context_labels_pending_setup_instead_of_a_fake_provider() {
        let context = BootContext::resolve(&super::AppLaunch {
            cwd: Some(std::path::PathBuf::from("C:/work/my-project")),
            resume: None,
        })
        .expect("explicit cwd resolves without touching the environment");
        assert_eq!(context.session, "new");
        assert!(context.provider.contains("setup pending"));
        let header = context.header();
        assert!(header.contains("Harness Agents"));
        assert!(header.contains("C:/work/my-project"));
        assert!(header.contains("/exit"));
    }

    #[test]
    fn h01_boot_context_marks_resume_as_pending_until_h05() {
        let context = BootContext::resolve(&super::AppLaunch {
            cwd: Some(std::path::PathBuf::from("C:/work/my-project")),
            resume: Some("session_01".to_owned()),
        })
        .expect("explicit cwd resolves without touching the environment");
        assert!(context.session.contains("session_01"));
        assert!(context.session.contains("pending H05"));
    }
}
