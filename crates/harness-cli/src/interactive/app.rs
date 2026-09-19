//! Interactive entrypoint host for `HA_LAUNCH` H01-H03.
//!
//! The host owns the terminal: it renders controller effects, feeds normalized
//! keys back, and restores the terminal through the raw-mode guard. The rules
//! live in the controller and are unit tested without a terminal; the very same
//! render loop runs against the scripted backend in tests.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use harness_types::{ErrorCode, HarnessError};

use super::bootstrap::{self, LaunchContext, LaunchRequest};
use super::controller::{Effect, InteractiveController};
use super::events::Key;
use super::paths::{HostPlatform, LaunchEnvironment};
use super::service::{AgentSessionService, FixtureService, SessionChannel, SessionPort};
use super::terminal::{CrosstermBackend, RawModeGuard, TerminalBackend};

/// Validated interactive launch request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppLaunch {
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
    /// Explicit opt-in to the labelled fixture backend.
    pub fixture: bool,
}

/// How long the render loop waits for a key before draining session events.
const KEY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Run the interactive app until the user exits.
pub async fn run(launch: AppLaunch) -> Result<ExitCode, HarnessError> {
    // The environment is read once and injected everywhere, so the same code path
    // is unit tested against a fixture environment.
    let environment = LaunchEnvironment::capture();
    let context = resolve_context(&launch, &environment)?;
    let notice = launch.resume.as_deref().map(|session| {
        format!("resume {session} requested; session recovery arrives with HA_LAUNCH H05")
    });
    match RawModeGuard::enter() {
        Ok(guard) => {
            let code = run_terminal(
                &context,
                &environment,
                guard,
                notice.as_deref(),
                launch.fixture,
            )?;
            Ok(ExitCode::from(code))
        }
        Err(error) => {
            eprintln!("ha: raw mode is unavailable ({error}); using plain line input");
            run_line_mode(&context, &environment, notice.as_deref(), launch.fixture).await
        }
    }
}

/// Resolve the launch context from the real environment.
///
/// The platform and environment are injected into the resolver, so the same code
/// path is unit tested with a fixture home instead of the developer profile.
fn resolve_context(
    launch: &AppLaunch,
    environment: &LaunchEnvironment,
) -> Result<LaunchContext, HarnessError> {
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
        environment: environment.clone(),
        explicit_data_dir: None,
    })
}

fn run_terminal(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    _guard: RawModeGuard,
    notice: Option<&str>,
    fixture: bool,
) -> Result<u8, HarnessError> {
    let mut backend = CrosstermBackend;
    let mut controller = controller_for(context, environment, fixture);
    run_loop(&mut backend, &mut controller, notice)
}

/// Build the controller.
///
/// Without the explicit fixture opt-in the backend is the real application
/// service. An unconfigured provider is reported as a setup error naming the
/// variables to set: a production launch never silently falls back to a fixture
/// or a mock.
fn controller_for(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    fixture: bool,
) -> InteractiveController {
    let channel = SessionChannel::new();
    let service: Box<dyn SessionPort> = if fixture {
        Box::new(FixtureService::new(channel.sender()))
    } else {
        Box::new(AgentSessionService::new(
            context,
            environment.clone(),
            channel.sender(),
        ))
    };
    InteractiveController::new(context, service, channel)
}

/// Whether the cursor sits at the start of a line, so partial output is never
/// overwritten by the next prompt.
#[derive(Clone, Copy, Debug, Default)]
struct RenderCursor {
    at_line_start: bool,
}

/// Render loop shared by the real terminal and the scripted test backend.
fn run_loop(
    backend: &mut impl TerminalBackend,
    controller: &mut InteractiveController,
    notice: Option<&str>,
) -> Result<u8, HarnessError> {
    let mut cursor = RenderCursor {
        at_line_start: true,
    };
    let mut boot = String::new();
    for line in controller.boot_lines() {
        boot.push_str(&line);
        boot.push_str("\r\n");
    }
    if let Some(notice) = notice {
        boot.push_str(notice);
        boot.push_str("\r\n");
    }
    backend
        .write(&boot)
        .map_err(|error| terminal_error(&error))?;
    draw_prompt(backend, controller, &mut cursor)?;
    loop {
        if backend
            .poll_key(KEY_POLL_INTERVAL)
            .map_err(|error| terminal_error(&error))?
        {
            let key = backend.read_key().map_err(|error| terminal_error(&error))?;
            if matches!(key, Key::Resize { .. }) {
                // A resize must not corrupt the prompt: redraw it in place and
                // keep waiting for real input.
                draw_prompt(backend, controller, &mut cursor)?;
            } else {
                let effects = controller.handle_key(key);
                if step(backend, controller, effects, &mut cursor)? {
                    return Ok(exit_code(&mut cursor));
                }
            }
        }
        let effects = controller.pump_events();
        if step(backend, controller, effects, &mut cursor)? {
            return Ok(exit_code(&mut cursor));
        }
    }
}

/// Apply one batch of effects; the returned flag means the app must exit.
fn step(
    backend: &mut impl TerminalBackend,
    controller: &InteractiveController,
    effects: Vec<Effect>,
    cursor: &mut RenderCursor,
) -> Result<bool, HarnessError> {
    let mut redraw = false;
    let mut exit = None;
    for effect in effects {
        match effect {
            Effect::WriteLine(line) => {
                if !cursor.at_line_start {
                    backend
                        .write("\r\n")
                        .map_err(|error| terminal_error(&error))?;
                }
                backend
                    .write(&line)
                    .map_err(|error| terminal_error(&error))?;
                backend
                    .write("\r\n")
                    .map_err(|error| terminal_error(&error))?;
                cursor.at_line_start = true;
            }
            Effect::WritePartial(text) => {
                backend
                    .write(&text)
                    .map_err(|error| terminal_error(&error))?;
                cursor.at_line_start = false;
            }
            Effect::RedrawPrompt => redraw = true,
            Effect::Exit(code) => exit = Some(code),
        }
    }
    backend.flush().map_err(|error| terminal_error(&error))?;
    if let Some(code) = exit {
        exit_cleanup(backend, cursor)?;
        return Ok(code == 0);
    }
    if redraw {
        draw_prompt(backend, controller, cursor)?;
    }
    Ok(false)
}

fn exit_code(cursor: &mut RenderCursor) -> u8 {
    cursor.at_line_start = true;
    0
}

/// Leave the prompt line cleanly before the shell prompt returns.
fn exit_cleanup(
    backend: &mut impl TerminalBackend,
    cursor: &mut RenderCursor,
) -> Result<(), HarnessError> {
    if !cursor.at_line_start {
        backend
            .write("\r\n")
            .map_err(|error| terminal_error(&error))?;
    }
    backend.flush().map_err(|error| terminal_error(&error))
}

fn draw_prompt(
    backend: &mut impl TerminalBackend,
    controller: &InteractiveController,
    cursor: &mut RenderCursor,
) -> Result<(), HarnessError> {
    if !cursor.at_line_start {
        backend
            .write("\r\n")
            .map_err(|error| terminal_error(&error))?;
    }
    backend
        .clear_line()
        .map_err(|error| terminal_error(&error))?;
    backend
        .write(&controller.prompt())
        .map_err(|error| terminal_error(&error))?;
    backend.flush().map_err(|error| terminal_error(&error))?;
    cursor.at_line_start = false;
    Ok(())
}

fn terminal_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageWriteFailed,
        format!("terminal input/output failed: {error}"),
    )
}

/// Line-mode fallback used when raw mode cannot be enabled.
///
/// It drives the same controller one submitted line at a time, so the rules and
/// the staged backend stay identical between the two modes.
async fn run_line_mode(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    notice: Option<&str>,
    fixture: bool,
) -> Result<ExitCode, HarnessError> {
    let mut controller = controller_for(context, environment, fixture);
    let mut output = std::io::stdout();
    for line in controller.boot_lines() {
        writeln!(output, "{line}").map_err(|error| io_error(&error))?;
    }
    if let Some(notice) = notice {
        writeln!(output, "{notice}").map_err(|error| io_error(&error))?;
    }
    write!(output, "{}", controller.prompt()).map_err(|error| io_error(&error))?;
    output.flush().map_err(|error| io_error(&error))?;

    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Option<String>>(16);
    std::thread::spawn(move || {
        use std::io::BufRead;

        let stdin = std::io::stdin();
        let mut handle = stdin.lock();
        let mut line = String::new();
        loop {
            line.clear();
            match handle.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = sender.blocking_send(None);
                    return;
                }
                Ok(_) => {
                    let text = line.trim_end_matches(['\r', '\n']).to_owned();
                    if sender.blocking_send(Some(text)).is_err() {
                        return;
                    }
                }
            }
        }
    });

    while let Some(line) = receiver.recv().await {
        let Some(line) = line else {
            writeln!(output, "bye").map_err(|error| io_error(&error))?;
            return Ok(ExitCode::SUCCESS);
        };
        for character in line.chars() {
            let _ = controller.handle_key(Key::Char(character));
        }
        if let Some(code) = render_line_mode(&mut controller, &mut output)? {
            return Ok(ExitCode::from(code));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn render_line_mode(
    controller: &mut InteractiveController,
    output: &mut impl Write,
) -> Result<Option<u8>, HarnessError> {
    let mut effects = controller.handle_key(Key::Enter);
    effects.extend(controller.pump_events());
    let mut exit = None;
    for effect in effects {
        match effect {
            Effect::WriteLine(line) => {
                writeln!(output, "{line}").map_err(|error| io_error(&error))?;
            }
            Effect::WritePartial(text) => {
                write!(output, "{text}").map_err(|error| io_error(&error))?;
            }
            Effect::RedrawPrompt => {}
            Effect::Exit(code) => exit = Some(code),
        }
    }
    if exit.is_none() {
        write!(output, "{}", controller.prompt()).map_err(|error| io_error(&error))?;
    } else {
        writeln!(output).map_err(|error| io_error(&error))?;
    }
    output.flush().map_err(|error| io_error(&error))?;
    Ok(exit)
}

fn io_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::StorageWriteFailed,
        format!("interactive output could not be written: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::{controller_for, run_loop};
    use crate::interactive::bootstrap::{self, LaunchContext, LaunchRequest};
    use crate::interactive::controller::InteractiveController;
    use crate::interactive::events::Key;
    use crate::interactive::paths::{HostPlatform, LaunchEnvironment};
    use crate::interactive::service::{FixtureService, SessionChannel, SessionPort};
    use crate::interactive::terminal::ScriptedBackend;

    /// Fixture home and project, never the developer profile.
    fn context(configured: bool) -> (tempfile::TempDir, LaunchContext) {
        let temp = tempfile::tempdir().expect("temp root");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&home).expect("fixture home");
        std::fs::create_dir_all(&project).expect("fixture project");
        if configured {
            std::fs::write(home.join("config.toml"), "schema_version = 1\n")
                .expect("fixture config");
        }
        let context = bootstrap::resolve(LaunchRequest {
            cwd: None,
            caller_dir: project,
            platform: HostPlatform::current(),
            environment: LaunchEnvironment::from_pairs([
                ("HA_HOME", home.to_string_lossy().into_owned()),
                ("DEEPSEEK_API_KEY", "fixture-secret".to_owned()),
            ]),
            explicit_data_dir: None,
        })
        .expect("context resolves");
        (temp, context)
    }

    fn keys_of(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    fn environment(pairs: &[(&str, &str)]) -> LaunchEnvironment {
        LaunchEnvironment::from_pairs(pairs.iter().map(|(name, value)| (*name, *value)))
    }

    #[test]
    fn h03_scripted_terminal_renders_the_boot_header_and_exits_cleanly() {
        let (_temp, context) = context(false);
        let mut backend = ScriptedBackend::new(vec![Key::EndOfInput]);
        let environment = environment(&[]);
        let mut controller = controller_for(&context, &environment, false);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);

        let output = backend.output();
        assert!(output.contains("Harness Agents"), "{output}");
        assert!(output.contains("setup required"), "{output}");
        assert!(
            output.contains("setup required (no provider configured)"),
            "an unconfigured provider is stated, not hidden: {output}"
        );
        assert!(output.contains("> "), "the prompt is drawn: {output}");
        assert!(
            backend.cleared_lines() > 0,
            "the prompt line is redrawn rather than duplicated"
        );
    }

    #[test]
    fn h03_scripted_terminal_echoes_vietnamese_input_and_reports_connection_pending() {
        let (_temp, context) = context(true);
        let mut keys = keys_of("sửa lỗi parser");
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let mut backend = ScriptedBackend::new(keys);
        let mut controller = controller_for(&context, &environment(&[]), true);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);

        let output = backend.output();
        assert!(output.contains("> sửa lỗi parser"), "{output}");
        assert!(
            output.contains("fixture answer for: sửa lỗi parser"),
            "{output}"
        );
        assert!(
            backend
                .writes()
                .iter()
                .any(|write| write.starts_with("[run] accepted ")),
            "the run reports its admitted input id: {output}"
        );
    }

    #[test]
    fn h03_scripted_terminal_edits_with_backspace_before_submitting() {
        let (_temp, context) = context(true);
        let mut keys = keys_of("abx");
        keys.push(Key::Backspace);
        keys.push(Key::Char('c'));
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let mut backend = ScriptedBackend::new(keys);
        let mut controller = controller_for(&context, &environment(&[]), true);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);

        let output = backend.output();
        assert!(
            output.contains("fixture answer for: abc"),
            "exactly the edited buffer reached the backend: {output}"
        );
        assert!(
            !output.contains("fixture answer for: abx"),
            "the erased character never reached the backend: {output}"
        );
        assert!(output.contains("fixture (no model was called)"));
    }

    #[test]
    fn h03_resize_redraws_the_prompt_without_losing_the_buffer() {
        let (_temp, context) = context(true);
        let mut keys = keys_of("hi");
        keys.push(Key::Resize {
            columns: 120,
            rows: 40,
        });
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let mut backend = ScriptedBackend::new(keys);
        let mut controller = controller_for(&context, &environment(&[]), true);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);
        assert!(backend.output().contains("> hi"), "{}", backend.output());
        assert!(backend.cleared_lines() >= 3, "every redraw clears the line");
    }

    #[test]
    fn h03_fixture_run_renders_a_requested_failure_and_is_labelled() {
        let (_temp, context) = context(true);
        let channel = SessionChannel::new();
        let service: Box<dyn SessionPort> = Box::new(FixtureService::new(channel.sender()));
        let mut controller = InteractiveController::new(&context, service, channel);
        let boot = controller.boot_lines().join("\n");
        assert!(boot.contains("fixture (no model was called)"), "{boot}");

        let mut keys = keys_of("please fail");
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let mut backend = ScriptedBackend::new(keys);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);
        let output = backend.output();
        assert!(output.contains("[tool] search_text failed"), "{output}");
        assert!(
            output.contains("[run] failed: fixture failure requested by the prompt"),
            "{output}"
        );
    }
}
