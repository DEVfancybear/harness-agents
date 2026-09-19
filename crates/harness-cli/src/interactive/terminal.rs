//! Terminal backend: the only place that touches a real terminal.
//!
//! The backend is a trait so the controller and its tests never need a PTY, and
//! the real backend owns raw mode behind an RAII guard: normal quit, error return
//! and unwinding all restore the terminal. A process that is killed hard is
//! explicitly out of scope, as the plan requires.

use std::io::{self, Write};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{cursor, execute};

use super::events::Key;

/// Everything the render loop needs from a terminal.
pub trait TerminalBackend {
    fn write(&mut self, text: &str) -> io::Result<()>;
    /// Erase the current line and return the cursor to column zero.
    fn clear_line(&mut self) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    /// Wait up to the timeout for input, reporting whether a key is ready.
    fn poll_key(&mut self, timeout: Duration) -> io::Result<bool>;
    fn read_key(&mut self) -> io::Result<Key>;
}

/// Real terminal backed by crossterm.
#[derive(Debug, Default)]
pub struct CrosstermBackend;

/// Test-only fault injection for the I08 acceptance case: with
/// `HA_TEST_FAIL_AFTER_MS` set, the real backend starts failing once that many
/// milliseconds have passed since the first write. The seam exists only in debug
/// builds, so a shipped binary can never be told to fail this way.
#[cfg(debug_assertions)]
fn injected_fault() -> io::Result<()> {
    use std::sync::OnceLock;
    use std::time::Instant;

    static DEADLINE: OnceLock<Option<Instant>> = OnceLock::new();
    let deadline = DEADLINE.get_or_init(|| {
        std::env::var("HA_TEST_FAIL_AFTER_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|millis| Instant::now() + Duration::from_millis(millis))
    });
    match deadline {
        Some(deadline) if Instant::now() >= *deadline => Err(io::Error::other(
            "injected terminal fault (HA_TEST_FAIL_AFTER_MS)",
        )),
        _ => Ok(()),
    }
}

#[cfg(not(debug_assertions))]
fn injected_fault() -> io::Result<()> {
    Ok(())
}

impl TerminalBackend for CrosstermBackend {
    fn write(&mut self, text: &str) -> io::Result<()> {
        injected_fault()?;
        io::stdout().write_all(text.as_bytes())
    }

    fn clear_line(&mut self) -> io::Result<()> {
        execute!(
            io::stdout(),
            Clear(ClearType::CurrentLine),
            cursor::MoveToColumn(0)
        )
    }

    fn flush(&mut self) -> io::Result<()> {
        injected_fault()?;
        io::stdout().flush()
    }

    fn poll_key(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read_key(&mut self) -> io::Result<Key> {
        Ok(map_event(event::read()?))
    }
}

/// The terminal modes the app owns. Injected so that restoration is provable in a
/// test process, which has no console to put into raw mode, and so that the guard
/// never depends on the real terminal being reachable at drop time.
trait ModeControl: std::fmt::Debug {
    fn enable(&self) -> io::Result<()>;
    fn disable(&self);
    fn enable_paste(&self);
    fn disable_paste(&self);
}

/// The real terminal: raw mode plus bracketed paste.
#[derive(Debug, Default)]
struct SystemModes;

impl ModeControl for SystemModes {
    fn enable(&self) -> io::Result<()> {
        terminal::enable_raw_mode()
    }

    fn disable(&self) {
        let _ = terminal::disable_raw_mode();
    }

    fn enable_paste(&self) {
        let _ = execute!(io::stdout(), event::EnableBracketedPaste);
    }

    fn disable_paste(&self) {
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
    }
}

/// Raw mode plus bracketed paste, restored when the guard is dropped.
#[derive(Debug)]
pub struct RawModeGuard {
    modes: Box<dyn ModeControl>,
    active: bool,
}

impl RawModeGuard {
    /// Enter raw mode; the caller must keep the guard alive for the whole session.
    pub fn enter() -> io::Result<Self> {
        Self::enter_with(Box::new(SystemModes))
    }

    /// Enter raw mode against an injected mode controller; the test seam for I08.
    fn enter_with(modes: Box<dyn ModeControl>) -> io::Result<Self> {
        modes.enable()?;
        // Bracketed paste is best effort: a terminal that does not support it must
        // not stop the app from starting.
        modes.enable_paste();
        Ok(Self {
            modes,
            active: true,
        })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        // Order matters: paste mode off, then raw mode, so a terminal never keeps
        // interpreting pasted bytes as commands.
        self.modes.disable_paste();
        self.modes.disable();
        self.active = false;
    }
}

fn map_event(event: Event) -> Key {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => map_key(key),
        Event::Paste(text) => Key::Paste(text),
        Event::Resize(columns, rows) => Key::Resize { columns, rows },
        _ => Key::Unknown,
    }
}

fn map_key(key: KeyEvent) -> Key {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if control => Key::Interrupt,
        KeyCode::Char('d') if control => Key::EndOfInput,
        KeyCode::Char(character) => Key::Char(character),
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        _ => Key::Unknown,
    }
}

/// Deterministic backend used by the render-loop tests.
///
/// When the script is exhausted it reports Ctrl-D, so a test that leaves an empty
/// prompt ends the loop instead of hanging.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct ScriptedBackend {
    keys: std::collections::VecDeque<Key>,
    output: String,
    writes: Vec<String>,
    cleared_lines: usize,
}

#[cfg(test)]
impl ScriptedBackend {
    #[must_use]
    pub fn new(keys: Vec<Key>) -> Self {
        Self {
            keys: keys.into(),
            output: String::new(),
            writes: Vec::new(),
            cleared_lines: 0,
        }
    }

    #[must_use]
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Every individual write, so a test can assert on complete lines instead
    /// of substring matches inside the whole transcript.
    #[must_use]
    pub fn writes(&self) -> &[String] {
        &self.writes
    }

    #[must_use]
    pub const fn cleared_lines(&self) -> usize {
        self.cleared_lines
    }
}

#[cfg(test)]
impl TerminalBackend for ScriptedBackend {
    fn write(&mut self, text: &str) -> io::Result<()> {
        self.output.push_str(text);
        self.writes.push(text.to_owned());
        Ok(())
    }

    fn clear_line(&mut self) -> io::Result<()> {
        self.cleared_lines += 1;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn poll_key(&mut self, _timeout: Duration) -> io::Result<bool> {
        Ok(true)
    }

    fn read_key(&mut self) -> io::Result<Key> {
        Ok(self.keys.pop_front().unwrap_or(Key::EndOfInput))
    }
}

#[cfg(test)]
mod tests {
    use super::{Key, KeyEventKind, map_event, map_key};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventState, KeyModifiers};

    #[test]
    fn h03_keys_are_mapped_from_real_crossterm_events() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            map_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Key::Interrupt
        );
        assert_eq!(
            map_key(key(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Key::EndOfInput
        );
        assert_eq!(
            map_key(key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Key::Char('a')
        );
        assert_eq!(
            map_key(key(KeyCode::Char('ư'), KeyModifiers::NONE)),
            Key::Char('ư')
        );
        assert_eq!(
            map_key(key(KeyCode::Backspace, KeyModifiers::NONE)),
            Key::Backspace
        );
        assert_eq!(map_key(key(KeyCode::Enter, KeyModifiers::NONE)), Key::Enter);
        assert_eq!(map_key(key(KeyCode::Home, KeyModifiers::NONE)), Key::Home);
        assert_eq!(
            map_key(key(KeyCode::F(5), KeyModifiers::NONE)),
            Key::Unknown
        );
    }

    #[test]
    fn h03_paste_resize_and_key_release_are_handled() {
        assert_eq!(
            map_event(Event::Paste("multi\nline".to_owned())),
            Key::Paste("multi\nline".to_owned())
        );
        assert_eq!(
            map_event(Event::Resize(120, 40)),
            Key::Resize {
                columns: 120,
                rows: 40
            }
        );
        let release = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        assert_eq!(map_event(Event::Key(release)), Key::Unknown);
        assert_eq!(map_event(Event::FocusGained), Key::Unknown);
    }

    /// I08: the modes the app owns are restored on the way out, including when
    /// the render loop unwinds instead of returning normally.
    #[test]
    fn h07_i08_the_guard_restores_every_mode_it_turned_on() {
        let modes = std::sync::Arc::new(RecordingModes::default());
        {
            let _guard = super::RawModeGuard::enter_with(Box::new(SharedModes(
                std::sync::Arc::clone(&modes),
            )))
            .expect("raw mode is entered through the seam");
            assert_eq!(modes.events(), vec!["enable", "paste on"]);
        }
        assert_eq!(
            modes.events(),
            vec!["enable", "paste on", "paste off", "disable"],
            "paste mode is turned off before raw mode, and both are restored"
        );
    }

    #[test]
    fn h07_i08_an_unwinding_failure_still_restores_the_terminal() {
        let modes = std::sync::Arc::new(RecordingModes::default());
        let captured = std::sync::Arc::clone(&modes);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard =
                super::RawModeGuard::enter_with(Box::new(SharedModes(captured))).expect("raw mode");
            panic!("the render loop failed after the terminal was initialized");
        }));
        assert!(unwind.is_err(), "the failure is not swallowed by the guard");
        assert_eq!(
            modes.events(),
            vec!["enable", "paste on", "paste off", "disable"],
            "an unwinding failure restores the terminal exactly like a clean exit"
        );
    }

    #[test]
    fn h07_i08_a_terminal_that_refuses_raw_mode_is_reported_and_not_claimed_open() {
        let modes = std::sync::Arc::new(RecordingModes {
            fail_enable: true,
            ..RecordingModes::default()
        });
        let error =
            super::RawModeGuard::enter_with(Box::new(SharedModes(std::sync::Arc::clone(&modes))))
                .expect_err("a refused raw mode is an error, never a silent downgrade");
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert_eq!(
            modes.events(),
            vec!["enable"],
            "nothing is turned off twice"
        );
    }

    #[derive(Debug, Default)]
    struct RecordingModes {
        events: std::sync::Mutex<Vec<&'static str>>,
        fail_enable: bool,
    }

    impl RecordingModes {
        fn events(&self) -> Vec<&'static str> {
            self.events.lock().expect("recording lock").clone()
        }

        fn record(&self, event: &'static str) {
            self.events.lock().expect("recording lock").push(event);
        }
    }

    struct SharedModes(std::sync::Arc<RecordingModes>);

    impl std::fmt::Debug for SharedModes {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("SharedModes")
        }
    }

    impl super::ModeControl for SharedModes {
        fn enable(&self) -> std::io::Result<()> {
            self.0.record("enable");
            if self.0.fail_enable {
                return Err(std::io::Error::other("no console"));
            }
            Ok(())
        }

        fn disable(&self) {
            self.0.record("disable");
        }

        fn enable_paste(&self) {
            self.0.record("paste on");
        }

        fn disable_paste(&self) {
            self.0.record("paste off");
        }
    }
}
