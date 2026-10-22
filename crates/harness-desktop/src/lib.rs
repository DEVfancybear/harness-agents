//! The desktop the `computer` tool sees and drives.
//!
//! A screen capture of the primary display (with the pointer drawn in, as a person
//! sees it) and real input: the pointer moved to absolute screen pixels, buttons,
//! the wheel, keys and text. Every call works in physical pixels: the calling thread
//! is made per-monitor DPI aware for the call, so a 150% display is captured and
//! clicked at the same 1920x1080, never at a scaled 1280x720 that misses by a third.
//!
//! Windows only for now; elsewhere every call answers [`Error::Unsupported`].
//! Key names are parsed here on every platform, so they are tested everywhere.

#![deny(unsafe_code)]

use std::fmt;

#[cfg(windows)]
mod win32;

/// Why a desktop call did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This platform (or this build) cannot drive a desktop.
    Unsupported(String),
    /// The call was made and refused: a locked or secure desktop, input blocked by
    /// a window running elevated, a capture that failed.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(message) | Self::Failed(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One capture of the primary display, top row first, 4 bytes per pixel (RGBA).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// A key as a chord names it: a virtual key, or a character the keyboard layout
/// says how to press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A Windows virtual-key code; `extended` keys (arrows, Home, Delete, ...) are
    /// sent with the extended flag, which some programs read to tell them from the
    /// number pad.
    Virtual { code: u16, extended: bool },
    /// A character key (`/`, `[`, `;`): its virtual key depends on the layout.
    Char(char),
}

impl Key {
    const fn virtual_key(code: u16) -> Self {
        Self::Virtual {
            code,
            extended: false,
        }
    }

    const fn extended(code: u16) -> Self {
        Self::Virtual {
            code,
            extended: true,
        }
    }

    /// A modifier: held while the chord's other keys are pressed.
    #[must_use]
    pub fn is_modifier(self) -> bool {
        matches!(
            self,
            Self::Virtual {
                code: VK_SHIFT | VK_CONTROL | VK_MENU | VK_LWIN,
                ..
            }
        )
    }
}

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;
const VK_LWIN: u16 = 0x5B;

/// A key by name, in the spellings computer-use models write: xdotool's
/// (`Return`, `Page_Up`, `Control_L`, `super`) and the browser's (`Enter`,
/// `ArrowUp`, `PageDown`, `Meta`), any case. A single character names itself.
#[must_use]
pub fn parse_key(name: &str) -> Option<Key> {
    // A key named by a space is the space bar, not an empty name.
    let trimmed = match name.trim() {
        "" if !name.is_empty() => " ",
        trimmed => trimmed,
    };
    let mut chars = trimmed.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        return Some(match single {
            ' ' => Key::virtual_key(0x20),
            'a'..='z' => Key::virtual_key(u16::from(single.to_ascii_uppercase() as u8)),
            'A'..='Z' | '0'..='9' => Key::virtual_key(u16::from(single as u8)),
            other => Key::Char(other),
        });
    }
    let mut folded = trimmed.to_ascii_lowercase().replace(['_', '-', ' '], "");
    // `Control_L`, `Shift_R`, `Alt_L`, `Super_L`: one side is as good as the other.
    for side in ["l", "r"] {
        if let Some(base) = folded.strip_suffix(side)
            && matches!(base, "control" | "shift" | "alt" | "super" | "meta")
        {
            folded = base.to_owned();
        }
    }
    let folded = folded
        .strip_prefix("arrow")
        .filter(|rest| matches!(*rest, "up" | "down" | "left" | "right"))
        .map_or(folded.as_str(), |rest| rest);
    let key = match folded {
        "return" | "enter" | "kpenter" => Key::virtual_key(0x0D),
        "tab" => Key::virtual_key(0x09),
        "escape" | "esc" => Key::virtual_key(0x1B),
        "backspace" | "back" => Key::virtual_key(0x08),
        "space" | "spacebar" => Key::virtual_key(0x20),
        "delete" | "del" => Key::extended(0x2E),
        "insert" | "ins" => Key::extended(0x2D),
        "home" => Key::extended(0x24),
        "end" => Key::extended(0x23),
        "pageup" | "prior" | "pgup" => Key::extended(0x21),
        "pagedown" | "next" | "pgdn" => Key::extended(0x22),
        "up" => Key::extended(0x26),
        "down" => Key::extended(0x28),
        "left" => Key::extended(0x25),
        "right" => Key::extended(0x27),
        "ctrl" | "control" => Key::virtual_key(VK_CONTROL),
        "shift" => Key::virtual_key(VK_SHIFT),
        "alt" | "option" => Key::virtual_key(VK_MENU),
        "super" | "win" | "windows" | "meta" | "cmd" | "command" => Key::extended(VK_LWIN),
        "capslock" | "caps" => Key::virtual_key(0x14),
        "numlock" => Key::extended(0x90),
        "scrolllock" => Key::virtual_key(0x91),
        "printscreen" | "print" | "prtsc" => Key::extended(0x2C),
        "pause" | "break" => Key::virtual_key(0x13),
        "apps" | "contextmenu" | "menu" => Key::extended(0x5D),
        "minus" => Key::Char('-'),
        "plus" => Key::Char('+'),
        "equal" | "equals" => Key::Char('='),
        "comma" => Key::Char(','),
        "period" | "dot" => Key::Char('.'),
        "slash" => Key::Char('/'),
        "backslash" => Key::Char('\\'),
        "semicolon" => Key::Char(';'),
        "apostrophe" | "quote" => Key::Char('\''),
        "grave" | "backtick" => Key::Char('`'),
        "bracketleft" => Key::Char('['),
        "bracketright" => Key::Char(']'),
        other => {
            let number = other.strip_prefix('f')?.parse::<u16>().ok()?;
            if !(1..=24).contains(&number) {
                return None;
            }
            Key::virtual_key(0x70 + number - 1)
        }
    };
    Some(key)
}

/// A chord as `ctrl+shift+t`: its keys in order, modifiers first as written.
/// `ctrl++` is Ctrl and the plus key.
pub fn parse_chord(chord: &str) -> std::result::Result<Vec<Key>, String> {
    let trimmed = match chord.trim() {
        "" if !chord.is_empty() => " ",
        trimmed => trimmed,
    };
    if trimmed.is_empty() {
        return Err("a key chord needs at least one key".to_owned());
    }
    let mut names: Vec<&str> = Vec::new();
    let mut rest = trimmed;
    while !rest.is_empty() {
        match rest.find('+') {
            // A `+` at the start of what is left is the plus key itself.
            Some(0) => {
                names.push("+");
                rest = rest[1..].strip_prefix('+').unwrap_or(&rest[1..]);
            }
            Some(at) => {
                names.push(&rest[..at]);
                rest = &rest[at + 1..];
            }
            None => {
                names.push(rest);
                rest = "";
            }
        }
    }
    let mut keys = Vec::with_capacity(names.len());
    for name in names {
        let key = parse_key(name).ok_or_else(|| format!("unknown key {name:?}"))?;
        if keys.contains(&key) {
            return Err(format!("{name:?} is named twice in {trimmed:?}"));
        }
        keys.push(key);
    }
    if keys.len() > 8 {
        return Err("a chord holds at most 8 keys".to_owned());
    }
    Ok(keys)
}

/// The primary display's size in physical pixels.
pub fn screen_size() -> Result<(u32, u32)> {
    #[cfg(windows)]
    {
        win32::screen_size()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}

/// Capture the primary display, the pointer drawn where it is.
pub fn capture() -> Result<Frame> {
    #[cfg(windows)]
    {
        win32::capture()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}

/// Where the pointer is, in physical pixels of the primary display.
pub fn cursor_position() -> Result<(i32, i32)> {
    #[cfg(windows)]
    {
        win32::cursor_position()
    }
    #[cfg(not(windows))]
    {
        Err(unsupported())
    }
}

/// Move the pointer to a physical pixel of the primary display.
pub fn move_pointer(x: i32, y: i32) -> Result<()> {
    #[cfg(windows)]
    {
        win32::move_pointer(x, y)
    }
    #[cfg(not(windows))]
    {
        let _ = (x, y);
        Err(unsupported())
    }
}

/// Press or release a mouse button where the pointer is.
pub fn button(button: Button, down: bool) -> Result<()> {
    #[cfg(windows)]
    {
        win32::button(button, down)
    }
    #[cfg(not(windows))]
    {
        let _ = (button, down);
        Err(unsupported())
    }
}

/// Turn the wheel by whole notches: positive is up (or right when `horizontal`).
pub fn scroll(notches: i32, horizontal: bool) -> Result<()> {
    #[cfg(windows)]
    {
        win32::scroll(notches, horizontal)
    }
    #[cfg(not(windows))]
    {
        let _ = (notches, horizontal);
        Err(unsupported())
    }
}

/// Press or release one key.
pub fn key(key: Key, down: bool) -> Result<()> {
    #[cfg(windows)]
    {
        win32::key(key, down)
    }
    #[cfg(not(windows))]
    {
        let _ = (key, down);
        Err(unsupported())
    }
}

/// Type one character as Unicode input (newline and tab as their keys).
///
/// Not as key presses: an input method turns keys into other text. Seen live: with
/// Vietnamese Telex on, typing `C:/Users` as keystrokes arrived as `C:/U Ú Ué ẻ é`.
/// Unicode input reaches a text field as the character itself, in any language;
/// programs that read key codes (games, shortcuts) take [`press_char`] or [`key`].
pub fn type_char(character: char) -> Result<()> {
    #[cfg(windows)]
    {
        win32::type_char(character)
    }
    #[cfg(not(windows))]
    {
        let _ = character;
        Err(unsupported())
    }
}

/// Press the key the keyboard layout has for a character (with Shift when the
/// layout needs it), as a person would: what games and shortcuts read.
pub fn press_char(character: char) -> Result<()> {
    #[cfg(windows)]
    {
        win32::press_char(character)
    }
    #[cfg(not(windows))]
    {
        let _ = character;
        Err(unsupported())
    }
}

/// The title of the window that has the keyboard, when there is one.
#[must_use]
pub fn foreground_title() -> Option<String> {
    #[cfg(windows)]
    {
        win32::foreground_title()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(not(windows))]
fn unsupported() -> Error {
    Error::Unsupported(
        "computer use drives a Windows desktop; this system is not Windows".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_read_in_the_spellings_models_write() {
        let enter = Some(Key::virtual_key(0x0D));
        for name in ["Return", "enter", "ENTER", "KP_Enter"] {
            assert_eq!(parse_key(name), enter, "{name}");
        }
        let up = Some(Key::extended(0x26));
        for name in ["Up", "ArrowUp", "arrow_up"] {
            assert_eq!(parse_key(name), up, "{name}");
        }
        assert_eq!(parse_key("Page_Down"), Some(Key::extended(0x22)));
        assert_eq!(parse_key("Control_L"), Some(Key::virtual_key(VK_CONTROL)));
        assert_eq!(parse_key("super"), Some(Key::extended(VK_LWIN)));
        assert_eq!(parse_key("space"), Some(Key::virtual_key(0x20)));
        assert_eq!(parse_key(" "), Some(Key::virtual_key(0x20)));
        assert_eq!(parse_key("a"), Some(Key::virtual_key(0x41)));
        assert_eq!(parse_key("Z"), Some(Key::virtual_key(0x5A)));
        assert_eq!(parse_key("7"), Some(Key::virtual_key(0x37)));
        assert_eq!(parse_key("F5"), Some(Key::virtual_key(0x74)));
        assert_eq!(parse_key("f24"), Some(Key::virtual_key(0x87)));
        assert_eq!(parse_key("slash"), Some(Key::Char('/')));
        assert_eq!(parse_key("/"), Some(Key::Char('/')));
        assert_eq!(parse_key("F25"), None);
        assert_eq!(parse_key("hyper"), None);
        assert_eq!(parse_key(""), None);
    }

    #[test]
    fn a_chord_is_its_keys_in_order() {
        assert_eq!(
            parse_chord("ctrl+shift+t"),
            Ok(vec![
                Key::virtual_key(VK_CONTROL),
                Key::virtual_key(VK_SHIFT),
                Key::virtual_key(0x54)
            ])
        );
        assert_eq!(
            parse_chord("ctrl++"),
            Ok(vec![Key::virtual_key(VK_CONTROL), Key::Char('+')])
        );
        assert_eq!(parse_chord("+"), Ok(vec![Key::Char('+')]));
        assert_eq!(parse_chord("space"), Ok(vec![Key::virtual_key(0x20)]));
        assert!(parse_chord("ctrl+ctrl").is_err());
        assert!(parse_chord("ctrl+nokey").is_err());
        assert!(parse_chord("").is_err());
        assert_eq!(parse_chord(" "), Ok(vec![Key::virtual_key(0x20)]));
        assert!(Key::virtual_key(VK_CONTROL).is_modifier());
        assert!(!Key::virtual_key(0x54).is_modifier());
    }
}
