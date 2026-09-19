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

impl TerminalBackend for CrosstermBackend {
    fn write(&mut self, text: &str) -> io::Result<()> {
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
        io::stdout().flush()
    }

    fn poll_key(&mut self, timeout: Duration) -> io::Result<bool> {
        event::poll(timeout)
    }

    fn read_key(&mut self) -> io::Result<Key> {
        Ok(map_event(event::read()?))
    }
}

/// Raw mode plus bracketed paste, restored when the guard is dropped.
#[derive(Debug)]
pub struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    /// Enter raw mode; the caller must keep the guard alive for the whole session.
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        // Bracketed paste is best effort: a terminal that does not support it must
        // not stop the app from starting.
        let _ = execute!(io::stdout(), event::EnableBracketedPaste);
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = execute!(io::stdout(), event::DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
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
}
