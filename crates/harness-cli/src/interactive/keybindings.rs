//! prime-agent's user keybindings (`keybindings.json`, its `keybindings.rs`):
//! `{ "<binding id>": "<key>" | ["<key>", ...] }` next to `settings.json`.
//! A configured binding answers to exactly the keys given - its default keys
//! stop triggering it - and an empty list turns it off. Keys are spelled as
//! prime-agent spells them (`ctrl+shift+z`, `alt+left`, `pageUp`), matched
//! case- and order-insensitively. ha honours the ids of the actions it has;
//! the others are ignored.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::events::Key;

/// The binding ids ha has an action for, with the action.
fn action(id: &str) -> Option<Key> {
    Some(match id {
        "tui.editor.cursorUp" => Key::Up,
        "tui.editor.cursorDown" => Key::Down,
        "tui.editor.cursorLeft" => Key::Left,
        "tui.editor.cursorRight" => Key::Right,
        "tui.editor.cursorWordLeft" => Key::WordLeft,
        "tui.editor.cursorWordRight" => Key::WordRight,
        "tui.editor.cursorLineStart" => Key::LineStart,
        "tui.editor.cursorLineEnd" => Key::LineEnd,
        "tui.editor.deleteCharBackward" => Key::Backspace,
        "tui.editor.deleteCharForward" => Key::Delete,
        "tui.editor.deleteWordBackward" => Key::EraseWord,
        "tui.editor.deleteWordForward" => Key::EraseWordForward,
        "tui.editor.deleteToLineStart" => Key::EraseToLineStart,
        "tui.editor.deleteToLineEnd" => Key::KillToLineEnd,
        "tui.editor.yank" => Key::Yank,
        "tui.editor.undo" => Key::Undo,
        "tui.editor.redo" => Key::Redo,
        "tui.editor.transposeChars" => Key::Transpose,
        "tui.input.newLine" => Key::Newline,
        "tui.input.submit" => Key::Enter,
        "tui.input.tab" => Key::Tab,
        "tui.viewport.pageUp" => Key::PageUp,
        "tui.viewport.pageDown" => Key::PageDown,
        "tui.viewport.top" => Key::ViewportTop,
        "tui.viewport.follow" => Key::ViewportFollow,
        "app.clear" => Key::Interrupt,
        "app.exit" => Key::EndOfInput,
        "app.model.cycleForward" => Key::CycleModel { forward: true },
        "app.model.cycleBackward" => Key::CycleModel { forward: false },
        "app.tools.expand" => Key::CycleDetail,
        "app.editor.external" => Key::ExternalEditor,
        "app.prompt.stash" => Key::Stash,
        "app.clipboard.pasteImage" => Key::PasteImage,
        _ => return None,
    })
}

/// The binding id a default key's action belongs to.
fn id_of(key: &Key) -> Option<&'static str> {
    Some(match key {
        Key::Up => "tui.editor.cursorUp",
        Key::Down => "tui.editor.cursorDown",
        Key::Left => "tui.editor.cursorLeft",
        Key::Right => "tui.editor.cursorRight",
        Key::WordLeft => "tui.editor.cursorWordLeft",
        Key::WordRight => "tui.editor.cursorWordRight",
        Key::LineStart | Key::Home => "tui.editor.cursorLineStart",
        Key::LineEnd | Key::End => "tui.editor.cursorLineEnd",
        Key::Backspace => "tui.editor.deleteCharBackward",
        Key::Delete => "tui.editor.deleteCharForward",
        Key::EraseWord => "tui.editor.deleteWordBackward",
        Key::EraseWordForward => "tui.editor.deleteWordForward",
        Key::EraseToLineStart => "tui.editor.deleteToLineStart",
        Key::KillToLineEnd => "tui.editor.deleteToLineEnd",
        Key::Yank => "tui.editor.yank",
        Key::Undo => "tui.editor.undo",
        Key::Redo => "tui.editor.redo",
        Key::Transpose => "tui.editor.transposeChars",
        Key::Newline => "tui.input.newLine",
        Key::Enter => "tui.input.submit",
        Key::Tab => "tui.input.tab",
        Key::PageUp => "tui.viewport.pageUp",
        Key::PageDown => "tui.viewport.pageDown",
        Key::ViewportTop => "tui.viewport.top",
        Key::ViewportFollow => "tui.viewport.follow",
        Key::Interrupt => "app.clear",
        Key::EndOfInput => "app.exit",
        Key::CycleModel { forward: true } => "app.model.cycleForward",
        Key::CycleModel { forward: false } => "app.model.cycleBackward",
        Key::CycleDetail => "app.tools.expand",
        Key::ExternalEditor => "app.editor.external",
        Key::Stash => "app.prompt.stash",
        Key::PasteImage => "app.clipboard.pasteImage",
        _ => return None,
    })
}

/// One key with its modifiers, as prime-agent's `parseKeyId` reads it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Chord {
    ctrl: bool,
    alt: bool,
    shift: bool,
    super_: bool,
    key: String,
}

impl Chord {
    /// `ctrl+shift+z`, in any order and case. `None` for an empty key.
    fn parse(spelled: &str) -> Option<Self> {
        let spelled = spelled.trim().to_lowercase();
        let mut chord = Self {
            ctrl: false,
            alt: false,
            shift: false,
            super_: false,
            key: String::new(),
        };
        // `ctrl++` would name the plus key; a trailing `+` is the key itself.
        let (modifiers, key) = match spelled.strip_suffix("++") {
            Some(head) => (head, "+"),
            None => spelled.rsplit_once('+').unwrap_or(("", spelled.as_str())),
        };
        for modifier in modifiers.split('+').filter(|part| !part.is_empty()) {
            match modifier {
                "ctrl" | "control" => chord.ctrl = true,
                "alt" | "option" | "meta" => chord.alt = true,
                "shift" => chord.shift = true,
                "super" | "cmd" | "command" => chord.super_ = true,
                _ => return None,
            }
        }
        if key.is_empty() {
            return None;
        }
        chord.key = match key {
            "esc" => "escape".to_owned(),
            "return" => "enter".to_owned(),
            other => other.to_owned(),
        };
        Some(chord)
    }

    /// The chord a terminal key event is.
    fn of(event: &KeyEvent) -> Option<Self> {
        let modifiers = event.modifiers;
        let mut shift = modifiers.contains(KeyModifiers::SHIFT);
        let key = match event.code {
            KeyCode::Char(' ') => "space".to_owned(),
            KeyCode::Char(character) => {
                if character.is_uppercase() {
                    shift = true;
                }
                character.to_lowercase().collect()
            }
            KeyCode::Left => "left".to_owned(),
            KeyCode::Right => "right".to_owned(),
            KeyCode::Up => "up".to_owned(),
            KeyCode::Down => "down".to_owned(),
            KeyCode::Home => "home".to_owned(),
            KeyCode::End => "end".to_owned(),
            KeyCode::PageUp => "pageup".to_owned(),
            KeyCode::PageDown => "pagedown".to_owned(),
            KeyCode::Enter => "enter".to_owned(),
            KeyCode::Tab => "tab".to_owned(),
            KeyCode::BackTab => {
                shift = true;
                "tab".to_owned()
            }
            KeyCode::Backspace => "backspace".to_owned(),
            KeyCode::Delete => "delete".to_owned(),
            KeyCode::Insert => "insert".to_owned(),
            KeyCode::Esc => "escape".to_owned(),
            KeyCode::F(number) => format!("f{number}"),
            _ => return None,
        };
        Some(Self {
            ctrl: modifiers.contains(KeyModifiers::CONTROL),
            alt: modifiers.contains(KeyModifiers::ALT),
            shift,
            super_: modifiers.intersects(KeyModifiers::SUPER | KeyModifiers::META),
            key,
        })
    }
}

/// The user's bindings: which ids they configured, and the keys of each.
#[derive(Debug, Default)]
pub struct Bindings {
    configured: BTreeSet<&'static str>,
    keys: Vec<(Chord, &'static str)>,
}

impl Bindings {
    /// Read `keybindings.json`; a missing or malformed file binds nothing, a
    /// malformed value drops, as prime-agent's `toKeybindingsConfig` does.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .map(|value| Self::from_json(&value))
            .unwrap_or_default()
    }

    fn from_json(value: &serde_json::Value) -> Self {
        let mut bindings = Self::default();
        let Some(entries) = value.as_object() else {
            return bindings;
        };
        for (id, keys) in entries {
            // The `&'static` id of an action ha has; others are ignored.
            let Some(id) = action(id)
                .and_then(|key| id_of(&key))
                .filter(|known| known == id)
            else {
                continue;
            };
            let spelled = match keys {
                serde_json::Value::String(key) => vec![key.as_str()],
                serde_json::Value::Array(keys) => {
                    keys.iter().filter_map(serde_json::Value::as_str).collect()
                }
                _ => continue,
            };
            bindings.configured.insert(id);
            bindings.keys.extend(
                spelled
                    .into_iter()
                    .filter_map(Chord::parse)
                    .map(|chord| (chord, id)),
            );
        }
        bindings
    }

    /// The key a terminal event becomes: a user binding first; a default key
    /// whose action the user rebound answers to nothing.
    fn resolve(&self, event: &KeyEvent, default: Key) -> Key {
        if let Some(chord) = Chord::of(event)
            && let Some((_, id)) = self.keys.iter().find(|(bound, _)| *bound == chord)
            && let Some(key) = action(id)
        {
            return key;
        }
        if id_of(&default).is_some_and(|id| self.configured.contains(id)) {
            return Key::Unknown;
        }
        default
    }
}

static ACTIVE: OnceLock<Bindings> = OnceLock::new();

/// Load the user's bindings once for this process, from the directory of
/// `config_file`.
pub fn install(config_file: &Path) {
    let path = config_file.with_file_name("keybindings.json");
    let _ = ACTIVE.set(Bindings::load(&path));
}

/// The key `event` stands for, after the user's bindings.
#[must_use]
pub fn apply(event: &KeyEvent, default: Key) -> Key {
    match ACTIVE.get() {
        Some(bindings) if !bindings.configured.is_empty() => bindings.resolve(event, default),
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::{Bindings, Chord};
    use crate::interactive::events::Key;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_parse_in_any_order_and_case() {
        assert_eq!(Chord::parse("Ctrl+Shift+Z"), Chord::parse("shift+ctrl+z"));
        assert_eq!(
            Chord::parse("ctrl+shift+z"),
            Chord::of(&event(
                KeyCode::Char('Z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
        assert_eq!(
            Chord::parse("pageUp"),
            Chord::of(&event(KeyCode::PageUp, KeyModifiers::NONE))
        );
        assert!(Chord::parse("hyper+x").is_none());
    }

    #[test]
    fn a_rebound_action_leaves_its_default_keys() {
        let bindings = Bindings::from_json(&serde_json::json!({
            "app.editor.external": "ctrl+e",
            "tui.editor.yank": [],
            "app.unknown.thing": "ctrl+q",
        }));
        // The new key runs the action...
        assert_eq!(
            bindings.resolve(
                &event(KeyCode::Char('e'), KeyModifiers::CONTROL),
                Key::LineEnd
            ),
            Key::ExternalEditor
        );
        // ...its old key no longer does, and an emptied binding is off.
        assert_eq!(
            bindings.resolve(
                &event(KeyCode::Char('g'), KeyModifiers::CONTROL),
                Key::ExternalEditor
            ),
            Key::Unknown
        );
        assert_eq!(
            bindings.resolve(&event(KeyCode::Char('y'), KeyModifiers::CONTROL), Key::Yank),
            Key::Unknown
        );
        // Everything else keeps its default.
        assert_eq!(
            bindings.resolve(
                &event(KeyCode::Char('k'), KeyModifiers::CONTROL),
                Key::KillToLineEnd
            ),
            Key::KillToLineEnd
        );
    }
}
