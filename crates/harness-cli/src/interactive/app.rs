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
use super::view;

/// Validated interactive launch request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppLaunch {
    pub cwd: Option<PathBuf>,
    pub resume: Option<String>,
    /// Explicit opt-in to the labelled fixture backend.
    pub fixture: bool,
    /// Force the plain renderer instead of the TUI (`--plain` or `HA_UI=plain`).
    pub plain: bool,
    pub config_overrides: super::config::ConfigOverrides,
}

/// How long the render loop waits for a key before draining session events.
const KEY_POLL_INTERVAL: Duration = super::tui::POLL_INTERVAL;

/// The smallest console the TUI renderer accepts.
///
/// Below this the viewport cannot hold a composer, a live block and a status row,
/// so the host falls back to the plain renderer and says why.
const TUI_MIN_COLUMNS: u16 = 60;
const TUI_MIN_ROWS: u16 = 10;

/// Why the TUI renderer is not in use, or `None` when it is.
///
/// The decision is a pure function of the request and the environment so it can
/// be unit tested without a terminal, and so the reason can be printed verbatim.
#[must_use]
pub fn tui_fallback_reason(
    plain: bool,
    environment: &LaunchEnvironment,
    columns: u16,
    rows: u16,
) -> Option<String> {
    if plain {
        return Some("plain renderer requested (--plain or HA_UI=plain)".to_owned());
    }
    let term = environment.value("TERM").unwrap_or_default();
    if term == "dumb" {
        return Some("TERM=dumb: this terminal cannot position the cursor".to_owned());
    }
    if columns < TUI_MIN_COLUMNS || rows < TUI_MIN_ROWS {
        return Some(format!(
            "the console is {columns}x{rows}; the TUI needs at least {TUI_MIN_COLUMNS}x{TUI_MIN_ROWS}"
        ));
    }
    None
}

/// Run the interactive app until the user exits.
pub async fn run(launch: AppLaunch) -> Result<ExitCode, HarnessError> {
    super::terminal::install_panic_hook();
    // The environment is read once and injected everywhere, so the same code path
    // is unit tested against a fixture environment.
    let environment = LaunchEnvironment::capture();
    let context = resolve_context(&launch, &environment)?;
    let source = launch.resume.as_deref();
    match RawModeGuard::enter() {
        Ok(guard) => {
            let code = run_terminal(&context, &environment, guard, source, &launch)?;
            Ok(ExitCode::from(code))
        }
        Err(error) => {
            eprintln!("ha: raw mode is unavailable ({error}); using plain line input");
            run_line_mode(
                &context,
                &environment,
                source,
                launch.fixture,
                launch.config_overrides.clone(),
            )
            .await
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
    launch: &AppLaunch,
) -> Result<u8, HarnessError> {
    let mut backend = CrosstermBackend;
    let size = backend.size().unwrap_or((TUI_MIN_COLUMNS, TUI_MIN_ROWS));
    let fallback = tui_fallback_reason(launch.plain, environment, size.0, size.1);
    // The plain renderer is the pre-T02 host and stays authoritative: the TUI is
    // chosen only when the console can hold it, and every refusal says why.
    let mut controller = controller_for_with_overrides(
        context,
        environment,
        launch.fixture,
        fallback.is_some(),
        launch.config_overrides.clone(),
    );
    if let Some(reason) = &fallback {
        eprintln!("ha: using the plain renderer because {reason}");
        controller.set_fallback_reason(reason.clone());
        return run_loop(&mut backend, &mut controller, notice);
    }
    super::tui::run(backend, &mut controller, notice)
}

/// Build the controller.
///
/// Without the explicit fixture opt-in the backend is the real application
/// service. An unconfigured provider is reported as a setup error naming the
/// variables to set: a production launch never silently falls back to a fixture
/// or a mock.
#[cfg(test)]
fn controller_for(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    fixture: bool,
    plain: bool,
) -> InteractiveController {
    controller_for_with_overrides(
        context,
        environment,
        fixture,
        plain,
        super::config::ConfigOverrides::default(),
    )
}

fn controller_for_with_overrides(
    context: &LaunchContext,
    environment: &LaunchEnvironment,
    fixture: bool,
    plain: bool,
    config_overrides: super::config::ConfigOverrides,
) -> InteractiveController {
    let channel = SessionChannel::new();
    let service: Box<dyn SessionPort> = if fixture {
        Box::new(FixtureService::new(channel.sender()))
    } else {
        // prime-agent refreshes its model catalog in the background; the snapshot
        // compiled in keeps working when the download fails.
        super::providers::refresh_in_background(&context.paths.data_dir);
        Box::new(AgentSessionService::new_with_overrides(
            context,
            environment.clone(),
            channel.sender(),
            config_overrides,
        ))
    };
    InteractiveController::new(context, service, channel, plain)
        .with_continuations(super::bounds::continuations_from_environment(environment))
}

/// Whether the cursor sits at the start of a line, so partial output is never
/// overwritten by the next prompt.
#[derive(Clone, Copy, Debug, Default)]
struct RenderCursor {
    at_line_start: bool,
    prompt_visible: bool,
    /// How many rows the visible prompt spans, so a multi-row prompt can be
    /// erased before the next write instead of leaving continuation rows behind.
    prompt_rows: u16,
}

/// Render loop shared by the real terminal and the scripted test backend.
fn run_loop(
    backend: &mut impl TerminalBackend,
    controller: &mut InteractiveController,
    notice: Option<&str>,
) -> Result<u8, HarnessError> {
    let mut cursor = RenderCursor {
        at_line_start: true,
        prompt_visible: false,
        prompt_rows: 0,
    };
    if let Some(source) = notice {
        controller
            .resume_source(source)
            .map_err(|message| HarnessError::new(ErrorCode::InvalidPayload, message))?;
    }
    let mut boot = String::new();
    for line in controller.boot_lines() {
        boot.push_str(&line);
        boot.push_str("\r\n");
    }
    if let Some(notice) = notice {
        boot.push_str("Selected session ");
        boot.push_str(notice);
        boot.push_str("; the next request verifies and recovers its context.");
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
            Effect::History(item) => {
                clear_prompt(backend, cursor)?;
                if !cursor.at_line_start {
                    backend
                        .write("\r\n")
                        .map_err(|error| terminal_error(&error))?;
                }
                for line in view::plain_lines(&item) {
                    let line = terminal_safe(&line);
                    backend
                        .write(&line)
                        .map_err(|error| terminal_error(&error))?;
                    backend
                        .write("\r\n")
                        .map_err(|error| terminal_error(&error))?;
                }
                cursor.at_line_start = true;
            }
            Effect::Stream(text) => {
                clear_prompt(backend, cursor)?;
                let text = terminal_safe(&text);
                backend
                    .write(&text)
                    .map_err(|error| terminal_error(&error))?;
                cursor.at_line_start = false;
            }
            Effect::Thinking(_) | Effect::Reprint(_) | Effect::Bell | Effect::ClearViewport => {}
            Effect::Copy(_) => {
                backend
                    .write("/copy is available in TUI mode; the plain renderer does not access the clipboard\r\n")
                    .map_err(|error| terminal_error(&error))?;
            }
            Effect::Redraw => redraw = true,
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
    if !cursor.at_line_start && !cursor.prompt_visible {
        backend
            .write("\r\n")
            .map_err(|error| terminal_error(&error))?;
    }
    // Erase the prompt that is already on screen first: a multi-row prompt must
    // not leave its continuation rows behind when it shrinks or grows.
    erase_prompt(backend, cursor)?;
    let lines = controller.prompt_lines();
    for (index, line) in lines.iter().enumerate() {
        backend
            .write(line)
            .map_err(|error| terminal_error(&error))?;
        if index + 1 < lines.len() {
            backend
                .write("\r\n")
                .map_err(|error| terminal_error(&error))?;
        }
    }
    backend.flush().map_err(|error| terminal_error(&error))?;
    // Leave the cursor where the next character would be typed, which may be a
    // continuation row rather than the end of the prompt.
    let (row, column) = controller.prompt_cursor_cell();
    let back = (lines.len().saturating_sub(1)).saturating_sub(row);
    if back > 0 {
        backend
            .move_up(u16::try_from(back).unwrap_or(u16::MAX))
            .map_err(|error| terminal_error(&error))?;
    }
    if column > 0 {
        backend
            .write(&format!("\r\u{1b}[{column}C"))
            .map_err(|error| terminal_error(&error))?;
    }
    backend.flush().map_err(|error| terminal_error(&error))?;
    cursor.at_line_start = false;
    cursor.prompt_visible = true;
    cursor.prompt_rows = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    Ok(())
}

/// Remove the visible prompt, returning the cursor to its first row.
///
/// A single-row prompt is always cleared, even when nothing was drawn yet, because
/// that is what keeps a stale shell line from surviving under the first prompt.
fn erase_prompt(
    backend: &mut impl TerminalBackend,
    cursor: &mut RenderCursor,
) -> Result<(), HarnessError> {
    let rows = if cursor.prompt_visible {
        cursor.prompt_rows.max(1)
    } else {
        1
    };
    for row in 0..rows {
        if row > 0 {
            backend.move_up(1).map_err(|error| terminal_error(&error))?;
        }
        backend
            .clear_line()
            .map_err(|error| terminal_error(&error))?;
    }
    cursor.prompt_visible = false;
    cursor.at_line_start = true;
    cursor.prompt_rows = 0;
    Ok(())
}

fn clear_prompt(
    backend: &mut impl TerminalBackend,
    cursor: &mut RenderCursor,
) -> Result<(), HarnessError> {
    erase_prompt(backend, cursor)
}

fn terminal_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        // The terminal is a required host service for this interactive path.
        // Keep its legacy generic-failure exit code (1); storage failures remain
        // execution errors (4) and use StorageWriteFailed at their call sites.
        ErrorCode::MissingRequiredService,
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
    config_overrides: super::config::ConfigOverrides,
) -> Result<ExitCode, HarnessError> {
    let mut controller =
        controller_for_with_overrides(context, environment, fixture, true, config_overrides);
    if let Some(source) = notice {
        controller
            .resume_source(source)
            .map_err(|message| HarnessError::new(ErrorCode::InvalidPayload, message))?;
    }
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

    run_line_input(&mut controller, &mut output, &mut receiver).await
}

async fn run_line_input(
    controller: &mut InteractiveController,
    output: &mut impl Write,
    receiver: &mut tokio::sync::mpsc::Receiver<Option<String>>,
) -> Result<ExitCode, HarnessError> {
    let mut ticks = tokio::time::interval(KEY_POLL_INTERVAL);
    loop {
        let effects = tokio::select! {
            line = receiver.recv() => match line.flatten() {
                Some(line) => {
                    for character in line.chars() {
                        let _ = controller.handle_key(Key::Char(character));
                    }
                    controller.handle_key(Key::Enter)
                }
                None => {
                    // Line input has no partial editor buffer. EOF follows
                    // the same cancel/exit route as an explicit quit.
                    controller.handle_key(Key::EndOfInput)
                }
            },
            _ = ticks.tick() => controller.pump_events(),
        };
        if let Some(code) = render_line_mode(controller, output, effects)? {
            return Ok(ExitCode::from(code));
        }
    }
}

fn render_line_mode(
    controller: &mut InteractiveController,
    output: &mut impl Write,
    effects: Vec<Effect>,
) -> Result<Option<u8>, HarnessError> {
    if effects.is_empty() {
        return Ok(None);
    }
    let mut exit = None;
    let mut redraw = false;
    for effect in effects {
        match effect {
            Effect::History(item) => {
                for line in view::plain_lines(&item) {
                    writeln!(output, "{}", terminal_safe(&line))
                        .map_err(|error| io_error(&error))?;
                }
            }
            Effect::Stream(text) => {
                write!(output, "{}", terminal_safe(&text)).map_err(|error| io_error(&error))?;
            }
            Effect::Thinking(_) | Effect::Reprint(_) | Effect::Bell | Effect::ClearViewport => {}
            Effect::Copy(_) => writeln!(
                output,
                "/copy is available in TUI mode; the plain renderer does not access the clipboard"
            )
            .map_err(|error| io_error(&error))?,
            Effect::Redraw => redraw = true,
            Effect::Exit(code) => exit = Some(code),
        }
    }
    if exit.is_none() && redraw {
        write!(output, "{}", controller.prompt()).map_err(|error| io_error(&error))?;
    } else if exit.is_some() {
        writeln!(output).map_err(|error| io_error(&error))?;
    }
    output.flush().map_err(|error| io_error(&error))?;
    Ok(exit)
}

/// Terminal control characters are stripped at the write boundary: model text,
/// file content and tool output can carry ESC sequences that would retitle the
/// window, move the cursor or write the clipboard. Newlines and tabs stay.
fn terminal_safe(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .collect()
}

fn io_error(error: &std::io::Error) -> HarnessError {
    HarnessError::new(
        ErrorCode::MissingRequiredService,
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
    use crate::interactive::terminal::{ScriptedBackend, TerminalBackend};

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
    fn completion_typing_redraws_without_a_new_line_per_key() {
        let (_temp, context) = context(true);
        let mut controller = controller_for(&context, &environment(&[]), true, true);
        let mut cursor = super::RenderCursor::default();
        let mut backend = ScriptedBackend::new(Vec::new());
        super::draw_prompt(&mut backend, &controller, &mut cursor).expect("prompt");
        let before = backend.writes().len();
        for key in [
            Key::Char('s'),
            Key::Char('ử'),
            Key::Char('a'),
            Key::Backspace,
        ] {
            let effects = controller.handle_key(key);
            assert!(!super::step(&mut backend, &controller, effects, &mut cursor).expect("redraw"));
        }
        assert!(
            !backend.writes()[before..]
                .iter()
                .any(|part| part.contains('\n')),
            "redraws must stay on the prompt line: {:?}",
            backend.writes()
        );
    }

    #[test]
    fn completion_launch_resume_is_delivered_to_the_session_port() {
        use std::sync::{Arc, Mutex};
        struct Port(Arc<Mutex<Vec<Option<String>>>>);
        impl SessionPort for Port {
            fn label(&self) -> String {
                "resume test".to_owned()
            }
            fn submit(&mut self, _: crate::interactive::service::SubmitRequest) {}
            fn cancel(&mut self) {}
            fn resume(&mut self, source: Option<String>) -> Result<(), String> {
                self.0.lock().expect("resume log").push(source);
                Ok(())
            }
        }
        let (_temp, context) = context(true);
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut controller = InteractiveController::new(
            &context,
            Box::new(Port(Arc::clone(&log))),
            SessionChannel::new(),
            true,
        );
        let mut backend = ScriptedBackend::new(vec![Key::EndOfInput]);
        let source = "session_0192f0aa-bbcc-7ddd-8eee-000000000001";
        assert_eq!(
            run_loop(&mut backend, &mut controller, Some(source)).expect("launch"),
            0
        );
        assert_eq!(
            *log.lock().expect("resume log"),
            vec![Some(source.to_owned())]
        );
    }

    #[tokio::test]
    async fn completion_line_mode_streams_before_the_next_input_line() {
        use std::sync::{Arc, Mutex};
        #[derive(Clone)]
        struct Output(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Output {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("output").extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let (_temp, context) = context(true);
        let channel = SessionChannel::new();
        let events = channel.sender();
        let mut controller = InteractiveController::new(
            &context,
            Box::new(FixtureService::new(events.clone())),
            channel,
            true,
        );
        let _ = controller.boot_lines();
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let mut output = Output(Arc::clone(&buffer));
        let (input, mut receiver) = tokio::sync::mpsc::channel(2);
        let producer = async {
            events
                .send(crate::interactive::events::SessionEvent::TextDelta {
                    text: "late response".to_owned(),
                })
                .expect("response");
            let observed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if String::from_utf8_lossy(&buffer.lock().expect("output"))
                        .contains("late response")
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .is_ok();
            input
                .send(Some("/exit".to_owned()))
                .await
                .expect("exit line");
            observed
        };
        let (result, observed) = tokio::join!(
            super::run_line_input(&mut controller, &mut output, &mut receiver),
            producer
        );
        assert_eq!(result.expect("line mode"), std::process::ExitCode::SUCCESS);
        assert!(
            observed,
            "output must arrive while stdin remains idle and open"
        );
    }

    /// T07: the renderer choice is decided from the console size and the
    /// environment, so it can be tested without a terminal. The TUI is the
    /// default; every refusal names its reason.
    #[test]
    fn t07_the_renderer_choice_is_explainable() {
        let term = environment(&[("TERM", "xterm-256color")]);
        assert_eq!(
            super::tui_fallback_reason(false, &term, 110, 30),
            None,
            "a normal console gets the TUI"
        );
        assert!(
            super::tui_fallback_reason(true, &term, 110, 30)
                .expect("plain was requested")
                .contains("--plain")
        );
        assert!(
            super::tui_fallback_reason(false, &term, 40, 30)
                .expect("narrow console")
                .contains("40x30")
        );
        assert!(
            super::tui_fallback_reason(false, &term, 110, 6)
                .expect("short console")
                .contains("at least")
        );
        let dumb = environment(&[("TERM", "dumb")]);
        assert!(
            super::tui_fallback_reason(false, &dumb, 110, 30)
                .expect("dumb terminal")
                .contains("TERM=dumb")
        );

        // The scripted backend reports whatever size the test asks for.
        let small = ScriptedBackend::new(Vec::new()).with_size(40, 8);
        assert_eq!(small.size().expect("size"), (40, 8));
    }

    /// The launch is where the environment reaches the controller: a budget set in the
    /// shell has to arrive without every other layer knowing about it.
    #[test]
    fn h03_the_continuation_budget_comes_from_the_launch_environment() {
        let (_temp, context) = context(false);
        let default = controller_for(&context, &environment(&[]), true, true);
        assert_eq!(
            default.continuation_budget(),
            crate::interactive::bounds::DEFAULT_CONTINUATIONS
        );

        let asked = controller_for(
            &context,
            &environment(&[("HA_TURN_CONTINUATIONS", "2")]),
            true,
            true,
        );
        assert_eq!(asked.continuation_budget(), 2);

        // Zero is a real answer: the app stops at every bound and waits for the user,
        // which is how it behaved before continuations existed.
        let off = controller_for(
            &context,
            &environment(&[("HA_TURN_CONTINUATIONS", "0")]),
            true,
            true,
        );
        assert_eq!(off.continuation_budget(), 0);

        // A value that is not a number is a typo, so the default stands.
        let typo = controller_for(
            &context,
            &environment(&[("HA_TURN_CONTINUATIONS", "many")]),
            true,
            true,
        );
        assert_eq!(
            typo.continuation_budget(),
            crate::interactive::bounds::DEFAULT_CONTINUATIONS
        );
    }

    #[test]
    fn h03_scripted_terminal_renders_the_boot_header_and_exits_cleanly() {
        let (_temp, context) = context(false);
        let mut backend = ScriptedBackend::new(vec![Key::EndOfInput]);
        let environment = environment(&[]);
        let mut controller = controller_for(&context, &environment, false, true);
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
        let mut controller = controller_for(&context, &environment(&[]), true, true);
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
        let mut controller = controller_for(&context, &environment(&[]), true, true);
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
    fn h03_a_multiline_draft_submits_once_and_erases_its_extra_rows() {
        let (_temp, context) = context(true);
        let mut keys = keys_of("first line");
        keys.push(Key::Newline);
        keys.extend(keys_of("second line"));
        keys.push(Key::Enter);
        keys.push(Key::EndOfInput);
        let mut backend = ScriptedBackend::new(keys);
        let mut controller = controller_for(&context, &environment(&[]), true, true);
        let code = run_loop(&mut backend, &mut controller, None).expect("loop runs");
        assert_eq!(code, 0);

        let output = backend.output();
        // Both rows were drawn, and the continuation row carries no marker.
        assert!(output.contains("> first line"), "{output}");
        assert!(output.contains("second line"), "{output}");
        assert!(
            !output.contains("> second line"),
            "only the first row carries the marker: {output}"
        );

        // The whole draft reached the backend as ONE request, not one per row.
        assert!(
            output.contains("fixture answer for: first line\nsecond line"),
            "the rows were submitted as a single message: {output}"
        );

        // Growing to two rows and then erasing for the response moved the cursor
        // up, so a continuation row was never left behind on screen.
        assert!(
            backend.moved_up() > 0,
            "a multi-row prompt must be erased by moving up: {output}"
        );
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
        let mut controller = controller_for(&context, &environment(&[]), true, true);
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
        let mut controller = InteractiveController::new(&context, service, channel, true);
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
